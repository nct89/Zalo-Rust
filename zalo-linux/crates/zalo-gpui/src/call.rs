//! 1-1 voice / video calls.
//!
//! UI after legacy Electron client `plugins/zcall/ui` (itself measured on Zalo for
//! macOS): the incoming notice is its own small blue window (344 × 232, 290
//! with "Answer without camera"), the call is the "Zalo Call - <name>"
//! window (450 × 672 voice, 640 × 400 video) with the blurred avatar, ring,
//! green timer and the black 50 px bar (camera pill, end call, micro pill,
//! settings). Icons and sounds are Zalo's own, copied to
//! `assets/{images/zcall,sounds}`.
//!
//! Engine as `zcall-native --media zrtc --send-udp`: `EngineCore` control
//! plane, its `sendSignal` frames over `/api/voicecall/*`, voip controls from
//! `cmd 601`, and `ZrtcMediaBackend` for UDP (ZRTP, SRTP audio) wired to the
//! mic / speaker `AudioBridge`. Remote H.264 is assembled by the media
//! session and drawn on the call window. The Linux camera is ffmpeg + OpenH264
//! at 640×360, the same baseline profile Electron sends.

use std::collections::{BTreeSet, HashMap};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    div, hsla, img, prelude::*, px, rgb, size, transparent_black, App, AppContext, Bounds,
    ClickEvent, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, RenderImage, StatefulInteractiveElement, Styled, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowOptions,
};
use serde_json::Value;
use zcall_core::call_ui::UiAction;
use zcall_core::engine::{EngineCore, Media, NoopMedia, Ui};
use zcall_core::zrtc_backend::{poll_and_tick, SharedZrtcBackend, ZrtcMediaBackend};

use crate::fonts::FAMILY_SEMIBOLD;
use crate::shell::ShellView;

/// Frame sender of the call in progress (one call at a time).
static ACTIVE: Mutex<Option<Sender<Value>>> = Mutex::new(None);

/// One member in a group call (`EngineCore::members`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct MemberInfo {
    uid: u32,
    name: String,
    avatar: String,
    muted: bool,
    cam_off: bool,
    state: String,
}

/// Which tile is pinned in conference layout (`Local` = "You").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PinTarget {
    Local,
    Remote(u32),
}

/// Stage layout mode matching `plugins/zcall/ui/call.js`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VideoLayout {
    Pip,
    Split,
    Grid,
    Conference,
}

/// What the worker thread tells the windows.
#[derive(Debug, Default)]
struct Shared {
    /// `calling` / `ringing` / `incoming` / `connected`.
    state: String,
    connected_at: Option<Instant>,
    ended: bool,
    /// Why it ended / engine notice, shown under the avatar.
    notice: String,
    /// Diagnostics (media, signalling).
    debug: String,
    /// Latest remote picture per sender, paired with sender SSRC in SSRC order.
    pictures: Vec<(u32, crate::call_video::RemotePicture)>,
    /// Bumps when `pictures` changes so the window rebuilds the texture.
    picture_gen: u64,
    /// Latest UI render width in pixels per remote sender SSRC (for SFU VidQual).
    tile_widths: Vec<(u32, u32)>,
    /// Bumps whenever `tile_widths` changes.
    tile_gen: u64,
    /// Local camera is sending.
    camera_on: bool,
    /// Local microphone muted state (for group "You" badge).
    local_muted: bool,
    /// Group call. The local camera is a tile, not the 1-1 corner view.
    group: bool,
    /// Our uid, so an echoed camera is not drawn beside the local preview.
    local_uid: u32,
    /// Pre-encode camera picture for the local tile / pip.
    local_preview: Option<crate::call_video::RemotePicture>,
    /// Bumps when `local_preview` changes.
    local_preview_gen: u64,
    /// Local screen share is capturing. The button follows this, so a failed
    /// grab turns "Share" back off.
    sharing: bool,
    /// Selected X11 display / region / window for screen share.
    share_target: crate::call_screen::ShareTarget,
    /// Label of the active screen share target.
    share_label: String,
    /// Pulse mic level the worker last applied, so the open menu can follow it.
    mic_volume: Option<u8>,
    /// Resolved alias from `Ui::rename`.
    peer_name_override: Option<String>,
    /// 1-1 peer state chips from `Ui::peer`.
    peer_cam_off: bool,
    peer_muted: bool,
    peer_sharing: bool,
    /// Group roster tiles from `Ui::members`.
    members: Vec<MemberInfo>,
    /// Bumps whenever `members` changes.
    members_gen: u64,
    /// Active speakers in a group call (`Ui::speaking`).
    speaking: BTreeSet<u32>,
    /// Bumps whenever `speaking` changes.
    speaking_gen: u64,
    /// Peer/engine asked local encoder for a key frame (`Ui::request_key_frame`).
    key_requested: bool,
    /// Live speaker playout volume, 0..=100 (default 100).
    speaker_volume: u8,
    /// Pending speaker volume update for the worker (`bridge.set_speaker_volume`).
    speaker_volume_req: Option<u8>,
    /// One-shot request from "Test speaker/headphones" to play a chime on the selected sink.
    test_speaker_req: bool,
    /// Pending `(aec, noise_suppression)` update for the worker (`bridge.set_audio_processing`).
    audio_dsp_req: Option<(bool, bool)>,
    /// Latest live microphone level from `AudioBridge` (0 loudest .. 127 silence).
    live_mic_level: u8,
}

type AvatarPair = (Option<Arc<RenderImage>>, Option<Arc<RenderImage>>);

/// Snapshot of visible UI scalars so the 16 ms video poll skips idle repaints.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RenderKey {
    state: String,
    ended: bool,
    camera_on: bool,
    timer_sec: Option<u64>,
    hint_active: bool,
    has_msg: bool,
    mic_volume: u8,
    speaker_volume: u8,
    sharing: bool,
    mic_dots: u8,
    test_tone_step: u8,
    members_gen: u64,
    speaking_gen: u64,
    peer_cam_off: bool,
    peer_muted: bool,
    peer_sharing: bool,
    video_layout: VideoLayout,
    pinned: Option<PinTarget>,
    fullscreen: bool,
}
/// Peer shown in the call windows.
#[derive(Clone)]
struct Peer {
    name: String,
    avatar: Option<Arc<RenderImage>>,
    /// Tiny decode stretched over the window: a cheap blur.
    blur: Option<Arc<RenderImage>>,
}

/// One call window (the incoming notice when `popup`).
pub struct CallView {
    peer: Peer,
    video: bool,
    popup: bool,
    shared: Arc<Mutex<Shared>>,
    action_tx: Sender<UiAction>,
    focus: FocusHandle,
    muted: bool,
    speaker_off: bool,
    hint: Option<(String, Instant)>,
    sound: Sound,
    /// The notice handed the call to the call window: do not hang up on drop.
    handed_off: bool,
    ended_at: Option<Instant>,
    /// Microphone / speaker menu (the caret and the gear).
    menu: bool,
    /// Devices for that menu, and the config dir their choice is saved in.
    audio: AudioChoice,
    /// Remote pictures currently on screen, paired with sender SSRC.
    pictures: Vec<(u32, Arc<RenderImage>)>,
    /// Frames already replaced on screen. Their GPU textures stay alive until
    /// `GPU_PICTURE_HOLD` so Intel i915 is not told to delete an image the
    /// previous frame is still sampling.
    held_pictures: Vec<(Instant, Arc<RenderImage>)>,
    /// `Shared::picture_gen` last copied into `pictures`.
    picture_gen: u64,
    /// Mirrors `Shared::camera_on` for the camera pill.
    camera_on: bool,
    /// Group layout puts the local camera in the grid. 1-1 uses the corner pip.
    group: bool,
    /// Current 1-1 or group layout mode (`Pip`, `Split`, `Grid`, `Conference`).
    video_layout: VideoLayout,
    /// Pinned group tile when in `Conference` layout.
    pinned: Option<PinTarget>,
    /// Local camera texture.
    local_image: Option<Arc<RenderImage>>,
    /// `Shared::local_preview_gen` last copied.
    local_preview_gen: u64,
    /// Local screen-share toggle. The engine sends cmd 12044 for a group and 12064 for 1-1.
    sharing: bool,
    /// Screen-share target picker & confirmation modal (matching macOS/Windows).
    share_modal: bool,
    /// Available X11 screens / regions / windows shown in `share_modal`.
    share_targets: Vec<crate::call_screen::ShareTarget>,
    /// Currently highlighted index in `share_targets`.
    share_selected: usize,
    /// Active screen-share target label shown in the top live banner.
    share_label: String,
    /// Snapshot of group members and peer status from `Shared`.
    members: Vec<MemberInfo>,
    members_gen: u64,
    speaking: BTreeSet<u32>,
    speaking_gen: u64,
    local_uid: u32,
    peer_name_override: Option<String>,
    peer_cam_off: bool,
    peer_muted: bool,
    peer_sharing: bool,
    /// Cached member avatar thumbnails (`avatar_url -> (avatar_160, blur_6)`).
    member_avatars: HashMap<String, AvatarPair>,
    /// Last viewport `(w, h, n, layout, pinned)` reported to `Shared::tile_widths`.
    last_tile_layout: (u32, u32, usize, VideoLayout, Option<PinTarget>),
    /// Snapshot of visible UI scalars so the 16 ms video poll skips idle repaints.
    last_render_key: RenderKey,
    /// The mic slider is being dragged. Device clicks must not close over it.
    mic_drag: bool,
    /// macOS-style "Device Status" modal opened by the gear icon.
    device_status_open: bool,
    /// Expanded dropdown inside "Device Status" (`"mic"`, `"speaker"`, `"camera"`).
    ds_dropdown: Option<&'static str>,
    /// Speaker output volume (0..=100) shown in "Device Status".
    speaker_volume: u8,
    /// Speaker volume slider is being dragged.
    spk_drag: bool,
    /// Latest live microphone level (0 loudest .. 127 silence) for the 16-dot meter.
    live_mic_level: u8,
    /// Start instant of the active "Test speaker/headphones" test chime (~1.6 s).
    test_tone_at: Option<Instant>,
}

/// PulseAudio / V4L2 devices shown in the call window.
#[derive(Clone)]
struct AudioChoice {
    mics: Vec<(String, String)>,
    speakers: Vec<(String, String)>,
    /// Label strings from [`crate::call_camera::camera_option_labels`].
    cameras: Vec<String>,
    /// Selected source name. Empty is the system default.
    mic: String,
    /// Selected sink name. Empty is the system default.
    speaker: String,
    /// Preferred `/dev/video*` path or label from `"cameraDevice"`.
    camera: String,
    /// Saved `"micVolume"`, 0..=100. Unset is 100 (0 dB, full scale).
    mic_volume: u8,
    /// Saved `"callAec"` (WebRTC AEC3, default `true`).
    call_aec: bool,
    /// Saved `"callNoiseSuppression"` (RNNoise AI denoiser, default `true`).
    call_ns: bool,
    /// Custom incoming ringtone (`linuxRingtone`). Empty uses Zalo's file.
    ringtone: String,
    /// Custom outgoing ringback (`linuxRingback`). Empty uses Zalo's file.
    ringback: String,
    dir: Option<std::path::PathBuf>,
}

impl Default for AudioChoice {
    fn default() -> Self {
        Self {
            mics: Vec::new(),
            speakers: Vec::new(),
            cameras: Vec::new(),
            mic: String::new(),
            speaker: String::new(),
            camera: String::new(),
            mic_volume: 100,
            call_aec: true,
            call_ns: true,
            ringtone: String::new(),
            ringback: String::new(),
            dir: None,
        }
    }
}

impl AudioChoice {
    fn from_prefs(prefs: &crate::prefs::Prefs, dir: Option<std::path::PathBuf>) -> Self {
        let mics = pulse_devices("sources");
        let speakers = pulse_devices("sinks");
        let cameras = crate::call_camera::camera_option_labels();
        Self {
            mic: saved_device(prefs, &["micDevice", "micSource"], &mics),
            speaker: saved_device(prefs, &["speakerDevice", "speakerSink"], &speakers),
            camera: prefs.str("cameraDevice", ""),
            mic_volume: mic_percent_from_json(prefs.json("micVolume").as_ref()),
            call_aec: prefs.bool("callAec", true),
            call_ns: prefs.bool("callNoiseSuppression", true),
            ringtone: prefs.str("linuxRingtone", ""),
            ringback: prefs.str("linuxRingback", ""),
            mics,
            speakers,
            cameras,
            dir,
        }
    }

    fn parec(&self) -> Option<String> {
        device_name(&self.mic)
    }

    fn pacat(&self) -> Option<String> {
        device_name(&self.speaker)
    }

    fn video_device(&self) -> Option<String> {
        crate::call_camera::parse_camera_path(&self.camera)
    }
}

/// Element ids cannot contain `:` from a Bluetooth node name.
fn device_row_id(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn device_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value == "Mặc định" {
        None
    } else {
        Some(value.to_owned())
    }
}

/// `"micVolume"` as 0..=100. Missing or out of range is full scale (100).
pub(crate) fn mic_percent_from_json(value: Option<&Value>) -> u8 {
    value
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().filter(|n| *n >= 0).map(|n| n as u64))
        })
        .map(|n| n.min(100) as u8)
        .unwrap_or(100)
}

fn pulse_source_name(pref: &str) -> String {
    let pref = pref.trim();
    if pref.is_empty() || pref == "Mặc định" {
        "@DEFAULT_SOURCE@".to_owned()
    } else {
        pref.to_owned()
    }
}

/// Writes the Pulse/PipeWire source level. No-op in tests. Does not mark ClipGuard manual;
/// a live call goes through [`send_mic_volume`] so the guard stops stepping.
pub(crate) fn spawn_pulse_volume(source_pref: &str, percent: u8) {
    if cfg!(test) || cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return;
    }
    let source = pulse_source_name(source_pref);
    let pct = percent.min(100);
    let raw = zcall_core::clip_guard::percent_to_raw(pct).to_string();
    let _ = std::thread::Builder::new()
        .name("zalo-mic-vol".into())
        .spawn(move || {
            let pactl_ok = Command::new("pactl")
                .args(["set-source-volume", &source, &raw])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !pactl_ok {
                let target = if source == "@DEFAULT_SOURCE@" {
                    "@DEFAULT_AUDIO_SOURCE@".to_owned()
                } else {
                    pipewire_node_id(&source).unwrap_or_else(|| "@DEFAULT_AUDIO_SOURCE@".to_owned())
                };
                let frac = format!("{:.2}", f64::from(pct) / 100.0);
                let _ = Command::new("wpctl")
                    .args(["set-volume", &target, &frac])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        });
}

fn pipewire_node_id(node_name: &str) -> Option<String> {
    let out = Command::new("pw-dump")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let list = serde_json::from_slice::<Vec<Value>>(&out.stdout).ok()?;
    for item in list {
        if item.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        if item.pointer("/info/props/node.name").and_then(Value::as_str) == Some(node_name) {
            if let Some(id) = item.get("id").and_then(Value::as_u64) {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Applies `micVolume` (or 100 when unset) to the saved microphone.
///
/// The clip guard can leave the camera mic at its −24 dB floor. 100% is 0 dB,
/// the normal full level, not a boost past unity.
pub fn restore_saved_mic_volume() {
    let prefs = crate::prefs::Prefs::load(crate::data::config_dir());
    let percent = mic_percent_from_json(prefs.json("micVolume").as_ref());
    spawn_pulse_volume(&prefs.str("micDevice", ""), percent);
}

/// Sends the level to the call in progress. `false` when no call is up.
pub(crate) fn send_mic_volume(percent: u8) -> bool {
    let tx = {
        let slot = MIC_TX.lock().unwrap_or_else(|err| err.into_inner());
        slot.as_ref().map(|(_, tx)| tx.clone())
    };
    match tx {
        Some(tx) => tx
            .send(UiAction::MicVolume {
                percent: i64::from(percent.min(100)),
            })
            .is_ok(),
        None => false,
    }
}

/// Settings and the call slider. A live call also stops the clip guard.
pub(crate) fn push_mic_volume(source_pref: &str, percent: u8) {
    let percent = percent.min(100);
    if !send_mic_volume(percent) {
        spawn_pulse_volume(source_pref, percent);
    }
}

/// One call at a time. The generation keeps a finished call from clearing the next.
static MIC_TX: Mutex<Option<(u64, Sender<UiAction>)>> = Mutex::new(None);

fn remember_mic_tx(tx: Sender<UiAction>) -> u64 {
    let mut slot = MIC_TX.lock().unwrap_or_else(|err| err.into_inner());
    let gen = slot.as_ref().map(|(g, _)| g.saturating_add(1)).unwrap_or(1);
    *slot = Some((gen, tx));
    gen
}

fn forget_mic_tx(gen: u64) {
    let mut slot = MIC_TX.lock().unwrap_or_else(|err| err.into_inner());
    if slot.as_ref().is_some_and(|(g, _)| *g == gen) {
        *slot = None;
    }
}

fn saved_device(prefs: &crate::prefs::Prefs, keys: &[&str], list: &[(String, String)]) -> String {
    for key in keys {
        let value = prefs.str(key, "");
        if list.iter().any(|(name, _)| name == &value) {
            return value;
        }
    }
    String::new()
}

/// `(name, description)` from PipeWire. Monitors and the internal headset
/// loopback are not choices. `kind` is `"sources"` or `"sinks"`.
pub(crate) fn is_selectable_pulse_device(name: &str) -> bool {
    !name.ends_with(".monitor")
        && !name.starts_with("zcall-ec-")
        && !name.starts_with("bluez_capture_internal.")
        && !name.starts_with("bluez_output_internal.")
}

pub(crate) fn pulse_devices(kind: &str) -> Vec<(String, String)> {
    if cfg!(test) {
        return Vec::new();
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        return zcall_audio::list_endpoints(kind);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
    let class = if kind == "sinks" {
        "Audio/Sink"
    } else {
        "Audio/Source"
    };
    if let Some(list) = pipewire_devices(class) {
        return list;
    }
    pactl_devices(kind)
    }
}

/// Nodes from `pw-dump`. `None` when the tool is missing or the dump is not JSON.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn pipewire_devices(class: &str) -> Option<Vec<(String, String)>> {
    let out = Command::new("pw-dump")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let list = serde_json::from_slice::<Vec<Value>>(&out.stdout).ok()?;
    let mut devices = Vec::new();
    for item in list {
        if item.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let props = item.pointer("/info/props")?;
        if props.get("media.class").and_then(Value::as_str) != Some(class) {
            continue;
        }
        let name = props.get("node.name").and_then(Value::as_str)?.to_owned();
        if !is_selectable_pulse_device(&name) {
            continue;
        }
        let desc = props
            .get("node.description")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| name.clone());
        devices.push((name, desc));
    }
    Some(devices)
}

/// `(name, description)` from `pactl -f json list`, when PipeWire's dump is unavailable.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn pactl_devices(kind: &str) -> Vec<(String, String)> {
    let Ok(out) = Command::new("pactl")
        .args(["-f", "json", "list", kind])
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let Ok(Value::Array(list)) = serde_json::from_slice::<Value>(&out.stdout) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|d| {
            let name = d.get("name")?.as_str()?.to_owned();
            if !is_selectable_pulse_device(&name) {
                return None;
            }
            let desc = d
                .get("description")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| name.clone());
            Some((name, desc))
        })
        .collect()
}

/// Looping / one-shot call sound decoded and played in-process (`symphonia` + `CardSpeaker`).
#[derive(Default)]
struct Sound {
    /// Resolved file path, or empty when silent. Changing it restarts playback.
    key: String,
    looped: bool,
    handle: Option<crate::audio_play::AudioHandle>,
}

impl Sound {
    fn set(&mut self, key: &str, path: Option<&std::path::Path>, looped: bool) {
        if self.key == key && self.looped == looped {
            return;
        }
        self.stop();
        self.key = key.to_owned();
        self.looped = looped;
        let Some(path) = path.filter(|p| p.is_file()) else {
            return;
        };
        self.handle = crate::audio_play::play_file(path.to_path_buf(), None, looped, None).ok();
    }

    fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.stop();
        }
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        // A one-shot end tone may finish on its own; loops must die.
        if self.looped {
            self.stop();
        }
    }
}

impl Drop for CallView {
    fn drop(&mut self) {
        if self.handed_off {
            return;
        }
        let over = self.shared.lock().map(|s| s.ended).unwrap_or(true);
        if !over {
            let _ = self.action_tx.send(if self.popup {
                UiAction::Reject {
                    why: "local".into(),
                }
            } else {
                UiAction::Hangup
            });
        }
    }
}

fn icon(name: &'static str, size_px: f32) -> gpui::AnyElement {
    match crate::media::png_asset(&format!("zcall/{name}"), 192) {
        Some(image) => img(image).size(px(size_px)).into_any_element(),
        None => div().size(px(size_px)).into_any_element(),
    }
}

impl ShellView {
    fn call_peer(&self, uid: &str, name: String) -> Peer {
        let url = self
            .profiles
            .map
            .get(uid)
            .map(|p| p.avatar.clone())
            .unwrap_or_default();
        let path = crate::media::cache_path_for(&url).filter(|p| p.is_file());
        Peer {
            name,
            avatar: path
                .as_ref()
                .and_then(|p| crate::media::decode_thumb(p, 160))
                .or_else(|| self.avatar_of(uid)),
            blur: path.as_ref().and_then(|p| crate::media::decode_thumb(p, 6)),
        }
    }

    /// Header ☎ / video, "Call back".
    pub(crate) fn start_call(&mut self, conv_id: u64, video: bool, cx: &mut Context<Self>) {
        let Some(conv) = self.conv(conv_id).cloned() else {
            return;
        };
        if !self.has_session || !conv.live {
            self.set_note("Chưa đăng nhập — không gọi được");
            return;
        }
        if ACTIVE.lock().map(|a| a.is_some()).unwrap_or(false) {
            self.set_note("Đang có cuộc gọi khác");
            return;
        }
        if conv.is_group {
            self.open_group_call(conv_id, video, cx);
            return;
        }
        let peer = self.call_peer(&conv.thread_id, conv.name.clone());
        let job = one_to_one_job(video, conv.thread_id.clone(), conv.name, false, None);
        let audio = AudioChoice::from_prefs(&self.prefs, self.session_dir.clone());
        let (shared, action_tx) = self.spawn_worker(job, "calling", &audio);
        open_call_window(cx, peer, video, shared, action_tx, audio);
    }

    /// 1-1 call to a uid that may not already be an open conversation.
    pub(crate) fn start_call_uid(
        &mut self,
        uid: String,
        name: String,
        video: bool,
        cx: &mut Context<Self>,
    ) {
        if uid.is_empty() {
            return;
        }
        if !self.has_session {
            self.set_note("Chưa đăng nhập — không gọi được");
            return;
        }
        if ACTIVE.lock().map(|a| a.is_some()).unwrap_or(false) {
            self.set_note("Đang có cuộc gọi khác");
            return;
        }
        let peer = self.call_peer(&uid, name.clone());
        let job = one_to_one_job(video, uid, name, false, None);
        let audio = AudioChoice::from_prefs(&self.prefs, self.session_dir.clone());
        let (shared, action_tx) = self.spawn_worker(job, "calling", &audio);
        open_call_window(cx, peer, video, shared, action_tx, audio);
    }

    /// Ids that are this account. `uin` is the login dkey, not a Zalo user id,
    /// so the signed-in member is also the profile whose name matches «me».
    pub(crate) fn self_member_ids(&self) -> std::collections::HashSet<String> {
        let mut ids = std::collections::HashSet::new();
        if let Some(uin) = self.uin.as_ref().filter(|id| !id.is_empty()) {
            ids.insert(uin.clone());
        }
        if let Some(id) = self.send2me_id.as_ref().filter(|id| !id.is_empty()) {
            ids.insert(id.clone());
        }
        let me_name = self
            .profiles
            .map
            .get(crate::profiles::ME)
            .map(|profile| profile.name.trim().to_owned())
            .unwrap_or_default();
        if !me_name.is_empty() {
            for (key, profile) in &self.profiles.map {
                if key != crate::profiles::ME
                    && !key.starts_with("g_")
                    && profile.name.trim() == me_name
                {
                    ids.insert(key.clone());
                }
            }
        }
        ids
    }

    /// Rings `ids` in the open group. An empty selection does not place a call.
    pub(crate) fn launch_group_call(
        &mut self,
        conv_id: u64,
        video: bool,
        ids: &[String],
        cx: &mut Context<Self>,
    ) {
        let Some(conv) = self.conv(conv_id).cloned() else {
            return;
        };
        if !self.has_session || !conv.live || !conv.is_group {
            self.set_note("Chưa đăng nhập — không gọi được");
            return;
        }
        if ACTIVE.lock().map(|a| a.is_some()).unwrap_or(false) {
            self.set_note("Đang có cuộc gọi khác");
            return;
        }
        let mine = self.self_member_ids();
        let (members, avatar) = self
            .profiles
            .map
            .get(&conv.thread_id)
            .map(|profile| (profile.members, profile.avatar.clone()))
            .unwrap_or((0, String::new()));
        let partners: Vec<Value> = ids
            .iter()
            .filter(|id| !id.is_empty() && !mine.contains(*id))
            .map(|id| {
                let name = self
                    .profiles
                    .map
                    .get(id)
                    .map(|person| person.name.clone())
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "Thành viên".to_owned());
                serde_json::json!({ "id": id, "name": name })
            })
            .collect();
        if partners.is_empty() {
            self.set_note("Chưa có danh sách thành viên để gọi nhóm");
            return;
        }
        let peer = self.call_peer(&conv.thread_id, conv.name.clone());
        let job = Job {
            video,
            peer_id: conv.thread_id.clone(),
            peer_name: conv.name,
            incoming: false,
            control: None,
            group: true,
            partners,
            max_users: members.max(2),
            avatar,
        };
        let audio = AudioChoice::from_prefs(&self.prefs, self.session_dir.clone());
        let (shared, action_tx) = self.spawn_worker(job, "calling", &audio);
        open_call_window(cx, peer, video, shared, action_tx, audio);
    }

    fn spawn_worker(
        &self,
        job: Job,
        state: &str,
        audio: &AudioChoice,
    ) -> (Arc<Mutex<Shared>>, Sender<UiAction>) {
        let (action_tx, action_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared {
            state: state.into(),
            camera_on: job.video,
            group: job.group,
            speaker_volume: 100,
            audio_dsp_req: Some((audio.call_aec, audio.call_ns)),
            live_mic_level: 127,
            ..Shared::default()
        }));
        let devices = (audio.parec(), audio.pacat());
        let camera = audio.video_device();
        let mic_volume = audio.mic_volume;
        let dir = self.session_dir.clone();
        if let Ok(mut active) = ACTIVE.lock() {
            *active = Some(frame_tx);
        }
        let for_worker = Arc::clone(&shared);
        let mic_gen = remember_mic_tx(action_tx.clone());
        let _ = std::thread::Builder::new()
            .name("zalo-gpui-call".into())
            .spawn(move || {
                worker(
                    &for_worker,
                    action_rx,
                    frame_rx,
                    job,
                    devices,
                    camera,
                    dir,
                    mic_volume,
                );
                forget_mic_tx(mic_gen);
                if let Ok(mut s) = for_worker.lock() {
                    s.ended = true;
                }
                if let Ok(mut active) = ACTIVE.lock() {
                    *active = None;
                }
            });
        (shared, action_tx)
    }

    /// Checks user call notification preferences before ringing an incoming call.
    fn accept_incoming_ring(&self, audio: &AudioChoice) -> bool {
        should_ring_incoming(
            self.prefs.bool("incomingCall", true),
            self.prefs.bool("needMic", false),
            self.prefs.bool("needSpeaker", false),
            !audio.mics.is_empty(),
            !audio.speakers.is_empty(),
        )
    }

    /// Realtime `cmd 601` voip controls: a new incoming call, or frames for
    /// the call in progress.
    pub(crate) fn on_voip(&mut self, controls: Vec<Value>, cx: &mut Context<Self>) {
        let now = crate::data::now_ms();
        let verbose = call_log_enabled();
        for ctrl in controls {
            let fresh = zalo_client::voip_control_is_fresh(&ctrl, now);
            let running = ACTIVE.lock().ok().and_then(|a| a.clone());
            if verbose {
                eprintln!(
                    "zalo-gpui call: on_voip fresh={fresh} active={} {}",
                    running.is_some(),
                    sanitized_call_json(&ctrl)
                );
            }
            if !fresh {
                continue;
            }
            if let Some(tx) = running {
                let _ = tx.send(zalo_client::control_frame(&ctrl));
                continue;
            }
            if ctrl.get("act").and_then(Value::as_str) == Some("group_request") {
                let audio = AudioChoice::from_prefs(&self.prefs, self.session_dir.clone());
                if !self.accept_incoming_ring(&audio) {
                    continue;
                }
                let data = ctrl.get("data").cloned().unwrap_or(serde_json::json!({}));
                let title = match data.get("groupName").and_then(Value::as_str) {
                    Some(name) if !name.is_empty() => name.to_owned(),
                    _ => "Cuộc gọi nhóm".to_owned(),
                };
                let thread = data
                    .get("groupId")
                    .and_then(|v| {
                        v.as_i64()
                            .map(|n| n.to_string())
                            .or_else(|| v.as_str().map(str::to_owned))
                    })
                    .map(|id| format!("g_{id}"))
                    .unwrap_or_default();
                let peer = self.call_peer(&thread, title.clone());
                let job = Job {
                    video: true,
                    peer_id: thread,
                    peer_name: title,
                    incoming: true,
                    control: Some(ctrl),
                    group: true,
                    partners: Vec::new(),
                    max_users: 0,
                    avatar: String::new(),
                };
                let (shared, action_tx) = self.spawn_worker(job, "incoming", &audio);
                open_incoming_window(cx, peer, true, shared, action_tx, audio);
                continue;
            }
            let Some(inc) = zalo_client::incoming_call_from_control(&ctrl) else {
                continue;
            };
            let audio = AudioChoice::from_prefs(&self.prefs, self.session_dir.clone());
            if !self.accept_incoming_ring(&audio) {
                continue;
            }
            let name = self
                .profiles
                .map
                .get(&inc.uid)
                .map(|p| p.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or(inc.name.clone());
            let peer = self.call_peer(&inc.uid, name.clone());
            let job = one_to_one_job(inc.video, inc.uid.clone(), name, true, Some(ctrl));
            let (shared, action_tx) = self.spawn_worker(job, "incoming", &audio);
            open_incoming_window(cx, peer, inc.video, shared, action_tx, audio);
        }
    }

    /// Dev hook `ZALO_GPUI_CALL=out|in|vin|up|vup`: the call windows with no
    /// worker and no signalling (nothing rings), for screenshots.
    pub(crate) fn demo_call(&mut self, mode: &str, cx: &mut Context<Self>) {
        let Some(conv) = self.selected_conv() else {
            return;
        };
        let peer = self.call_peer(&conv.thread_id, conv.name.clone());
        let (action_tx, _) = mpsc::channel();
        let up = mode.ends_with("up");
        let shared = Arc::new(Mutex::new(Shared {
            state: if up {
                "connected".into()
            } else {
                "ringing".into()
            },
            connected_at: up.then(|| Instant::now() - Duration::from_secs(3)),
            speaker_volume: 100,
            live_mic_level: 127,
            ..Shared::default()
        }));
        let video = mode.starts_with('v');
        let mode = mode.to_owned();
        cx.spawn(async move |_, cx| {
            let _ = cx.update(|cx| {
                if mode.ends_with("in") {
                    open_incoming_window(
                        cx,
                        peer,
                        video,
                        shared,
                        action_tx,
                        AudioChoice::default(),
                    );
                } else {
                    open_call_window(cx, peer, video, shared, action_tx, AudioChoice::default());
                }
            });
        })
        .detach();
    }
}

fn new_view(
    peer: Peer,
    video: bool,
    popup: bool,
    shared: Arc<Mutex<Shared>>,
    action_tx: Sender<UiAction>,
    audio: AudioChoice,
    cx: &mut Context<CallView>,
) -> CallView {
    let mic_volume = audio.mic_volume;
    let group = shared.lock().map(|s| s.group).unwrap_or(false);
    let video_layout = if group {
        VideoLayout::Grid
    } else {
        VideoLayout::Pip
    };
    CallView {
        peer,
        video,
        popup,
        shared,
        action_tx,
        focus: cx.focus_handle(),
        muted: false,
        speaker_off: false,
        hint: None,
        sound: Sound::default(),
        handed_off: false,
        ended_at: None,
        menu: false,
        audio,
        pictures: Vec::new(),
        held_pictures: Vec::new(),
        picture_gen: 0,
        camera_on: video && !popup,
        group,
        video_layout,
        pinned: None,
        local_image: None,
        local_preview_gen: 0,
        sharing: false,
        share_modal: false,
        share_targets: Vec::new(),
        share_selected: 0,
        share_label: String::new(),
        members: Vec::new(),
        members_gen: 0,
        speaking: BTreeSet::new(),
        speaking_gen: 0,
        local_uid: 0,
        peer_name_override: None,
        peer_cam_off: false,
        peer_muted: false,
        peer_sharing: false,
        member_avatars: HashMap::new(),
        last_tile_layout: (0, 0, 0, video_layout, None),
        last_render_key: RenderKey {
            state: String::new(),
            ended: false,
            camera_on: false,
            timer_sec: None,
            hint_active: false,
            has_msg: false,
            mic_volume,
            speaker_volume: 100,
            sharing: false,
            mic_dots: 0,
            test_tone_step: 0,
            members_gen: 0,
            speaking_gen: 0,
            peer_cam_off: false,
            peer_muted: false,
            peer_sharing: false,
            video_layout,
            pinned: None,
            fullscreen: false,
        },
        mic_drag: false,
        device_status_open: false,
        ds_dropdown: None,
        speaker_volume: 100,
        spk_drag: false,
        live_mic_level: 127,
        test_tone_at: None,
    }
}

/// How long a replaced call frame keeps its GPU texture. blade destroys the
/// VkImage inside `drop_image` immediately; Intel i915 hangs if that image is
/// still referenced by the previous frame.
const GPU_PICTURE_HOLD: Duration = Duration::from_millis(300);

/// Index of the first entry that is not yet due. Entries are in insertion order.
fn due_prefix<T>(items: &[(Instant, T)], now: Instant) -> usize {
    items
        .iter()
        .position(|(at, _)| *at > now)
        .unwrap_or(items.len())
}

/// 16 ms video presentation loop (~60 Hz for smooth 24-30 fps video; 50 ms on voice/popup).
/// Skips `cx.notify()` when neither video textures nor call state changed.
fn start_clock(window: &mut Window, cx: &mut Context<CallView>, fast_video: bool) {
    let weak = cx.weak_entity();
    let step = Duration::from_millis(if fast_video { 16 } else { 50 });
    window
        .spawn(cx, async move |cx| loop {
            gpui::Timer::after(step).await;
            let alive = weak.update_in(cx, |view, window, cx| {
                if view.tick(window) {
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();
}

/// The blue notice (`incoming.css`): 344 × 232, 290 for a video call.
/// The window is larger by the CSD shadow so the corners can be transparent.
fn open_incoming_window(
    cx: &mut App,
    peer: Peer,
    video: bool,
    shared: Arc<Mutex<Shared>>,
    action_tx: Sender<UiAction>,
    audio: AudioChoice,
) {
    let content_h = if video { 290. } else { 232. };
    let bounds = Bounds::centered(None, crate::window_frame::outer_size(344., content_h), cx);
    let _ = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            is_resizable: false,
            is_minimizable: false,
            window_decorations: Some(WindowDecorations::Client),
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("zalo-gpui".into()),
            ..Default::default()
        },
        |window, cx| {
            window.set_window_title("Zalo: cuộc gọi đến");
            cx.new(|cx| {
                start_clock(window, cx, false);
                new_view(peer, video, true, shared, action_tx, audio, cx)
            })
        },
    );
}

/// "Zalo Call - <name>": 450 × 672 voice, 760 × 480 video (fits >= 380 px group tiles).
fn open_call_window(
    cx: &mut App,
    peer: Peer,
    video: bool,
    shared: Arc<Mutex<Shared>>,
    action_tx: Sender<UiAction>,
    audio: AudioChoice,
) {
    let group = shared.lock().map(|s| s.group).unwrap_or(false);
    let wide = video || group;
    let (w, h) = if wide { (760., 480.) } else { (450., 672.) };
    let min_sz = if wide {
        size(px(560.), px(350.))
    } else {
        size(px(320.), px(240.))
    };
    let title = format!("Zalo Call - {}", peer.name);
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    let _ = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title.clone().into()),
                ..Default::default()
            }),
            is_resizable: true,
            window_decorations: Some(WindowDecorations::Server),
            window_min_size: Some(min_sz),
            app_id: Some("zalo-gpui".into()),
            ..Default::default()
        },
        |window, cx| {
            window.set_window_title(&title);
            let view = cx.new(|cx| {
                start_clock(window, cx, wide);
                new_view(peer, video, false, shared, action_tx, audio, cx)
            });
            window.focus(&view.read(cx).focus);
            view
        },
    );
}

impl CallView {
    fn snapshot(&self) -> (String, Option<Instant>, bool, String, String) {
        self.shared
            .lock()
            .map(|s| {
                (
                    s.state.clone(),
                    s.connected_at,
                    s.ended,
                    s.notice.clone(),
                    s.debug.clone(),
                )
            })
            .unwrap_or_default()
    }

    /// Ringtone, ringback, or the end tone. Custom paths come from Settings › Advanced.
    fn play_kind(&mut self, kind: &str, looped: bool) {
        let custom = match kind {
            "ringtone" => self.audio.ringtone.as_str(),
            "ringback" => self.audio.ringback.as_str(),
            _ => "",
        };
        let path = resolve_call_sound(kind, custom);
        let key = path
            .as_ref()
            .map(|p| format!("{kind}:{}", p.display()))
            .unwrap_or_default();
        self.sound.set(&key, path.as_deref(), looped);
    }

    fn tick(&mut self, window: &mut Window) -> bool {
        self.flush_held_pictures(window);
        let pics_changed = self.take_pictures();
        let (state, connected, ended, notice, debug) = self.snapshot();
        if let Ok(s) = self.shared.lock() {
            self.camera_on = s.camera_on;
            self.sharing = s.sharing;
            if s.share_label != self.share_label {
                self.share_label = s.share_label.clone();
            }
            self.live_mic_level = s.live_mic_level;
            self.local_uid = s.local_uid;
            if let Some(name) = s.peer_name_override.as_ref().filter(|n| !n.is_empty()) {
                self.peer_name_override = Some(name.clone());
            }
            self.peer_cam_off = s.peer_cam_off;
            self.peer_muted = s.peer_muted;
            self.peer_sharing = s.peer_sharing;
            if s.members_gen != self.members_gen {
                self.members_gen = s.members_gen;
                self.members = s.members.clone();
            }
            if s.speaking_gen != self.speaking_gen {
                self.speaking_gen = s.speaking_gen;
                self.speaking = s.speaking.clone();
            }
            if !self.mic_drag {
                if let Some(vol) = s.mic_volume {
                    self.audio.mic_volume = vol;
                }
            }
            if !self.spk_drag && s.speaker_volume != self.speaker_volume {
                self.speaker_volume = s.speaker_volume;
            }
        }
        if self
            .test_tone_at
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(1600))
        {
            self.test_tone_at = None;
        }
        self.sync_member_avatars();
        self.report_tile_widths(window);
        let timer_sec = connected.filter(|_| !ended).map(|t| t.elapsed().as_secs());
        let mic_dots = if self.device_status_open {
            mic_level_to_dots(self.live_mic_level, self.muted)
        } else {
            0
        };
        let test_tone_step = if self.device_status_open {
            self.test_tone_at
                .map(|at| ((at.elapsed().as_millis() as u64 * 16) / 1600).min(16) as u8)
                .unwrap_or(0)
        } else {
            0
        };
        let hint_active = self
            .hint
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(4));
        let has_msg = !notice.is_empty() || !debug.is_empty();
        let fullscreen = window.is_fullscreen();
        let next_key = RenderKey {
            state: state.clone(),
            ended,
            camera_on: self.camera_on,
            timer_sec,
            hint_active,
            has_msg,
            mic_volume: self.audio.mic_volume,
            speaker_volume: self.speaker_volume,
            sharing: self.sharing,
            mic_dots,
            test_tone_step,
            members_gen: self.members_gen,
            speaking_gen: self.speaking_gen,
            peer_cam_off: self.peer_cam_off,
            peer_muted: self.peer_muted,
            peer_sharing: self.peer_sharing,
            video_layout: self.video_layout,
            pinned: self.pinned,
            fullscreen,
        };
        let ui_changed = next_key != self.last_render_key;
        self.last_render_key = next_key;

        if ended {
            if self.popup {
                self.sound.stop();
                self.release_pictures();
                window.remove_window();
                return false;
            }
            let at = *self.ended_at.get_or_insert_with(Instant::now);
            self.play_kind("end", false);
            if at.elapsed() > Duration::from_millis(1800) {
                self.release_pictures();
                window.remove_window();
            }
            return pics_changed || ui_changed;
        }
        if self.popup {
            self.play_kind("ringtone", true);
        } else if connected.is_some() || state == "connected" || state == "failed" {
            self.sound.set("", None, false);
        } else {
            self.play_kind("ringback", true);
        }
        pics_changed || ui_changed
    }

    fn release_pictures(&mut self) {
        // The window renderer waits for the GPU, then destroys its atlas.
        // Dropping the textures here deletes images the last frame may still
        // be sampling.
        self.pictures.clear();
        self.held_pictures.clear();
    }

    /// Delete only the textures the GPU has had time to finish reading.
    fn flush_held_pictures(&mut self, window: &mut Window) {
        let ready = due_prefix(&self.held_pictures, Instant::now());
        for (_, image) in self.held_pictures.drain(..ready) {
            let _ = window.drop_image(image);
        }
    }

    fn take_pictures(&mut self) -> bool {
        let Ok(shared) = self.shared.lock() else {
            return false;
        };
        let pics = shared.picture_gen != self.picture_gen;
        let local = shared.local_preview_gen != self.local_preview_gen;
        if !pics && !local {
            return false;
        }
        if pics {
            self.picture_gen = shared.picture_gen;
            let hide_echo =
                shared.camera_on && shared.local_preview_gen > 0 && shared.local_uid != 0;
            let uid = shared.local_uid;
            let next = shared
                .pictures
                .iter()
                .filter(|(src, _)| !hide_echo || *src != uid)
                .filter_map(|(src, picture)| {
                    let (nw, nh, bytes) = crate::call_video::preview_bgra(picture)?;
                    let image = image::RgbaImage::from_raw(nw, nh, bytes)?;
                    Some((*src, Arc::new(RenderImage::new([image::Frame::new(image)]))))
                })
                .collect();
            drop(shared);
            let old = std::mem::replace(&mut self.pictures, next);
            let drop_at = Instant::now() + GPU_PICTURE_HOLD;
            self.held_pictures
                .extend(old.into_iter().map(|(_, image)| (drop_at, image)));
            if local {
                self.take_local_preview();
            }
            return true;
        }
        drop(shared);
        self.take_local_preview();
        true
    }

    fn take_local_preview(&mut self) {
        let Ok(mut shared) = self.shared.lock() else {
            return;
        };
        if shared.local_preview_gen == self.local_preview_gen {
            return;
        }
        self.local_preview_gen = shared.local_preview_gen;
        let preview = shared.local_preview.take();
        drop(shared);
        let next = preview.and_then(|picture| {
            let (nw, nh, bytes) = crate::call_video::preview_bgra(&picture)?;
            let image = image::RgbaImage::from_raw(nw, nh, bytes)?;
            Some(Arc::new(RenderImage::new([image::Frame::new(image)])))
        });
        if let Some(old) = self.local_image.take() {
            self.held_pictures
                .push((Instant::now() + GPU_PICTURE_HOLD, old));
        }
        self.local_image = next;
    }

    fn sync_member_avatars(&mut self) {
        for member in &self.members {
            if member.avatar.is_empty() || self.member_avatars.contains_key(&member.avatar) {
                continue;
            }
            let path = crate::media::cache_path_for(&member.avatar).filter(|p| p.is_file());
            let pair = (
                path.as_ref().and_then(|p| crate::media::decode_thumb(p, 160)),
                path.as_ref().and_then(|p| crate::media::decode_thumb(p, 6)),
            );
            self.member_avatars.insert(member.avatar.clone(), pair);
        }
    }

    fn display_peer_name(&self) -> &str {
        self.peer_name_override
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.peer.name)
    }

    fn toggle_layout(&mut self, cx: &mut Context<Self>) {
        if self.group {
            if self.video_layout == VideoLayout::Conference {
                self.video_layout = VideoLayout::Grid;
                self.pinned = None;
                self.say("Hiển thị dạng lưới");
            } else {
                self.video_layout = VideoLayout::Conference;
                if self.pinned.is_none() {
                    self.pinned = self
                        .members
                        .first()
                        .map(|m| PinTarget::Remote(m.uid))
                        .or_else(|| self.pictures.first().map(|(uid, _)| PinTarget::Remote(*uid)))
                        .or(Some(PinTarget::Local));
                }
                self.say("Hiển thị người nói chính");
            }
        } else {
            self.video_layout = if self.video_layout == VideoLayout::Split {
                self.say("Hiển thị người nói chính");
                VideoLayout::Pip
            } else {
                self.say("Hiển thị dạng lưới");
                VideoLayout::Split
            };
        }
        cx.notify();
    }

    fn toggle_pin(&mut self, target: PinTarget, cx: &mut Context<Self>) {
        if !self.group {
            return;
        }
        if self.video_layout == VideoLayout::Conference && self.pinned == Some(target) {
            self.pinned = None;
            self.video_layout = VideoLayout::Grid;
        } else {
            self.pinned = Some(target);
            self.video_layout = VideoLayout::Conference;
        }
        cx.notify();
    }

    fn handle_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let mods = &e.keystroke.modifiers;
        if mods.control || mods.alt || mods.platform || mods.function {
            return;
        }
        match e.keystroke.key.as_str() {
            "f11" if (self.video || self.group) => {
                window.toggle_fullscreen();
                cx.stop_propagation();
                cx.notify();
            }
            "escape" => {
                if self.menu {
                    self.menu = false;
                    cx.stop_propagation();
                    cx.notify();
                } else if window.is_fullscreen() {
                    window.toggle_fullscreen();
                    cx.stop_propagation();
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// Ordered group tiles (`You` + active/ringing members + any unrostered senders).
    fn group_tiles(&self) -> Vec<StageTile> {
        let mut tiles = Vec::new();
        let local_spk = self.local_uid != 0 && self.speaking.contains(&self.local_uid);
        tiles.push(StageTile {
            target: PinTarget::Local,
            name: "Bạn".to_owned(),
            avatar: None,
            blur: None,
            status: if self.camera_on {
                String::new()
            } else {
                "Đã tắt camera".to_owned()
            },
            image: if self.camera_on || self.sharing {
                self.local_image.clone()
            } else {
                None
            },
            muted: self.muted,
            speaking: local_spk,
            contain: self.sharing && !self.camera_on,
        });
        let mut seen_uids = BTreeSet::new();
        for m in &self.members {
            seen_uids.insert(m.uid);
            let pic = self
                .pictures
                .iter()
                .find(|(uid, _)| *uid == m.uid)
                .map(|(_, img)| Arc::clone(img));
            let (avatar, blur) = self
                .member_avatars
                .get(&m.avatar)
                .cloned()
                .unwrap_or((None, None));
            let status = match m.state.as_str() {
                "ringing" => "Đang đổ chuông...".to_owned(),
                "calling" => "Đang gọi...".to_owned(),
                _ if m.cam_off || pic.is_none() => "Đã tắt camera".to_owned(),
                _ => String::new(),
            };
            tiles.push(StageTile {
                target: PinTarget::Remote(m.uid),
                name: if m.name.is_empty() {
                    format!("Thành viên {}", m.uid)
                } else {
                    m.name.clone()
                },
                avatar,
                blur,
                status,
                image: if m.cam_off { None } else { pic },
                muted: m.muted,
                speaking: self.speaking.contains(&m.uid),
                contain: false,
            });
        }
        for (uid, pic) in &self.pictures {
            if !seen_uids.contains(uid) && (self.local_uid == 0 || *uid != self.local_uid) {
                tiles.push(StageTile {
                    target: PinTarget::Remote(*uid),
                    name: format!("Thành viên {uid}"),
                    avatar: None,
                    blur: None,
                    status: String::new(),
                    image: Some(Arc::clone(pic)),
                    muted: false,
                    speaking: self.speaking.contains(uid),
                    contain: false,
                });
            }
        }
        tiles
    }

    fn report_tile_widths(&mut self, window: &Window) {
        if self.popup || (!self.group && self.pictures.is_empty()) {
            return;
        }
        let vp = window.viewport_size();
        let scale = window.scale_factor();
        let w_px = (f32::from(vp.width) * scale).round().max(0.) as u32;
        let h_px = (f32::from(vp.height) * scale).round().max(0.) as u32;
        if w_px == 0 {
            return;
        }
        let vp_w = f32::from(vp.width);
        let stage_h = (f32::from(vp.height) - 50.0).max(1.0);
        let widths: Vec<(u32, u32)> = if self.group {
            let tiles = self.group_tiles();
            let layout = (w_px, h_px, tiles.len(), self.video_layout, self.pinned);
            if layout == self.last_tile_layout {
                return;
            }
            self.last_tile_layout = layout;
            let main_idx = self
                .pinned
                .and_then(|p| tiles.iter().position(|t| t.target == p))
                .unwrap_or(0);
            let rects = if self.video_layout == VideoLayout::Conference && tiles.len() > 1 {
                group_conference_layout(vp_w, stage_h, tiles.len(), main_idx)
            } else {
                group_grid_layout(vp_w, stage_h, tiles.len())
            };
            tiles
                .iter()
                .zip(rects.iter())
                .filter_map(|(tile, rect)| match tile.target {
                    PinTarget::Remote(uid) => Some((uid, (rect.w * scale).round().max(160.) as u32)),
                    PinTarget::Local => None,
                })
                .collect()
        } else {
            let layout = (
                w_px,
                h_px,
                self.pictures.len(),
                self.video_layout,
                self.pinned,
            );
            if layout == self.last_tile_layout {
                return;
            }
            self.last_tile_layout = layout;
            let remote_w = if self.video_layout == VideoLayout::Split {
                ((vp_w - 4.0) * 0.5 * scale).round().max(160.) as u32
            } else {
                (vp_w * scale).round().max(160.) as u32
            };
            self.pictures
                .iter()
                .map(|(src, _)| (*src, remote_w))
                .collect()
        };
        if let Ok(mut shared) = self.shared.lock() {
            if shared.tile_widths != widths {
                shared.tile_widths = widths;
                shared.tile_gen = shared.tile_gen.saturating_add(1);
            }
        }
    }

    fn send(&mut self, action: UiAction) {
        let _ = self.action_tx.send(action);
    }

    fn say(&mut self, text: &str) {
        self.hint = Some((text.to_owned(), Instant::now()));
    }

    /// Notice › answer: open the call window on the same call, close this one.
    fn accept(&mut self, cam_off: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.send(UiAction::Accept {
            cam_start_off: cam_off,
        });
        self.handed_off = true;
        self.sound.stop();
        self.release_pictures();
        let (peer, video, shared, tx, audio) = (
            self.peer.clone(),
            self.video,
            Arc::clone(&self.shared),
            self.action_tx.clone(),
            self.audio.clone(),
        );
        cx.defer(move |cx| open_call_window(cx, peer, video, shared, tx, audio));
        window.remove_window();
    }

    fn save_device(&mut self, kind: &str, id: &str) {
        match kind {
            "mic" => self.audio.mic = id.to_owned(),
            "camera" => self.audio.camera = id.to_owned(),
            _ => self.audio.speaker = id.to_owned(),
        }
        let Some(dir) = self.audio.dir.clone() else {
            return;
        };
        let stored = if id.is_empty() {
            "Mặc định".to_owned()
        } else {
            id.to_owned()
        };
        let mut prefs = crate::prefs::Prefs::load(Some(dir));
        let value = Value::String(stored);
        match kind {
            "mic" => {
                prefs.set("micDevice", value.clone());
                prefs.set("micSource", value);
            }
            "camera" => prefs.set("cameraDevice", value),
            _ => {
                prefs.set("speakerDevice", value.clone());
                prefs.set("speakerSink", value);
            }
        }
    }

    fn refresh_devices(&mut self) {
        // The headset may have connected after the call window opened.
        self.audio.mics = pulse_devices("sources");
        self.audio.speakers = pulse_devices("sinks");
        self.audio.cameras = crate::call_camera::camera_option_labels();
    }

    fn toggle_menu(&mut self, cx: &mut Context<Self>) {
        if !self.menu {
            self.refresh_devices();
        }
        self.menu = !self.menu;
        if self.menu {
            self.device_status_open = false;
            self.ds_dropdown = None;
        }
        cx.notify();
    }

    fn toggle_device_status(&mut self, cx: &mut Context<Self>) {
        if !self.device_status_open {
            self.refresh_devices();
        }
        self.device_status_open = !self.device_status_open;
        self.ds_dropdown = None;
        if self.device_status_open {
            self.menu = false;
            self.share_modal = false;
        }
        cx.notify();
    }

    fn commit_speaker_volume(&mut self, value: u8, cx: &mut Context<Self>) {
        let pct = value.min(100);
        self.speaker_volume = pct;
        if pct > 0 && self.speaker_off {
            self.speaker_off = false;
            self.send(UiAction::Speaker { off: false });
        }
        if let Ok(mut s) = self.shared.lock() {
            s.speaker_volume = pct;
            s.speaker_volume_req = Some(pct);
        }
        cx.notify();
    }

    fn trigger_speaker_test(&mut self, cx: &mut Context<Self>) {
        if self.speaker_off {
            self.speaker_off = false;
            self.send(UiAction::Speaker { off: false });
        }
        self.test_tone_at = Some(Instant::now());
        if let Ok(mut s) = self.shared.lock() {
            s.test_speaker_req = true;
        }
        cx.notify();
    }

    fn toggle_audio_dsp(&mut self, toggle_aec: bool, cx: &mut Context<Self>) {
        if toggle_aec {
            self.audio.call_aec = !self.audio.call_aec;
        } else {
            self.audio.call_ns = !self.audio.call_ns;
        }
        let (aec, ns) = (self.audio.call_aec, self.audio.call_ns);
        if let Some(dir) = self.audio.dir.clone() {
            let mut prefs = crate::prefs::Prefs::load(Some(dir));
            prefs.set("callAec", Value::Bool(aec));
            prefs.set("callNoiseSuppression", Value::Bool(ns));
        }
        if let Ok(mut s) = self.shared.lock() {
            s.audio_dsp_req = Some((aec, ns));
        }
        let notice = if toggle_aec {
            if aec { "Đã bật khử vọng âm (AEC)" } else { "Đã tắt khử vọng âm (AEC)" }
        } else if ns {
            "Đã bật lọc tiếng ồn AI (RNNoise)"
        } else {
            "Đã tắt lọc tiếng ồn AI"
        };
        self.say(notice);
        cx.notify();
    }

    fn decline(&mut self, window: &mut Window) {
        self.send(UiAction::Reject {
            why: "local".into(),
        });
        self.handed_off = true;
        self.sound.stop();
        self.release_pictures();
        window.remove_window();
    }

    fn render_tile(
        &self,
        eid: (&'static str, usize),
        tile: &StageTile,
        rect: TileRect,
        pinned: bool,
        clickable: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let target = tile.target;
        let avatar_d = (rect.h * 0.35).clamp(36., 96.);
        let mut el = div()
            .id(eid)
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .overflow_hidden()
            .rounded(px(4.))
            .bg(rgb(0x23_23_23))
            .when(tile.speaking, |d| {
                d.border(px(1.5)).border_color(rgb(0x00_68_ff))
            })
            .when(!tile.speaking, |d| {
                d.border_1().border_color(hsla(0., 0., 1., 0.1))
            });
        if clickable {
            el = el.cursor_pointer().on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    this.toggle_pin(target, cx);
                },
            ));
        }
        let body = if let Some(image) = tile.image.clone() {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(0x00_00_00))
                .child(if tile.contain {
                    render_video_contain(image, rect.w, rect.h)
                } else {
                    render_video_fit(image, rect.w, rect.h)
                })
                .into_any_element()
        } else {
            div()
                .relative()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .when_some(tile.blur.clone(), |d, b| {
                    d.child(
                        img(b)
                            .absolute()
                            .inset(px(-24.))
                            .size_full()
                            .object_fit(gpui::ObjectFit::Cover),
                    )
                    .child(div().absolute().inset(px(0.)).bg(hsla(0., 0., 0., 0.55)))
                })
                .child(div().relative().child(avatar_circle(
                    &tile.name,
                    tile.avatar.clone(),
                    avatar_d,
                    2.,
                    hsla(0., 0., 1., 0.9),
                )))
                .when(!tile.status.is_empty(), |d| {
                    d.child(
                        div()
                            .relative()
                            .px(px(8.))
                            .text_size(px(12.))
                            .text_color(hsla(0., 0., 1., 0.8))
                            .child(tile.status.clone()),
                    )
                })
                .into_any_element()
        };
        el.child(body)
            .when(pinned, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(6.))
                        .right(px(6.))
                        .size(px(22.))
                        .rounded(px(11.))
                        .bg(hsla(0., 0., 0., 0.5))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon("pin.png", 12.)),
                )
            })
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .bottom(px(0.))
                    .max_w(px((rect.w - 8.).max(24.)))
                    .h(px(21.))
                    .px(px(6.))
                    .rounded_tr(px(4.))
                    .bg(hsla(0., 0., 0., 0.5))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .when(tile.muted, |d| d.child(icon("micoff.png", 13.)))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(12.))
                            .line_height(px(21.))
                            .text_color(rgb(0xff_ff_ff))
                            .child(tile.name.clone()),
                    ),
            )
            .into_any_element()
    }

    /// Stage video content: group grid/conference, 1-1 full/split, or pre-connect dimmed local.
    fn render_stage(&self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let vp = window.viewport_size();
        let vp_w = f32::from(vp.width).max(1.);
        let stage_h = (f32::from(vp.height) - 50.).max(1.);

        if self.group {
            let tiles = self.group_tiles();
            let main_idx = self
                .pinned
                .and_then(|p| tiles.iter().position(|t| t.target == p))
                .unwrap_or(0);
            let is_conf = self.video_layout == VideoLayout::Conference && tiles.len() > 1;
            let rects = if is_conf {
                group_conference_layout(vp_w, stage_h, tiles.len(), main_idx)
            } else {
                group_grid_layout(vp_w, stage_h, tiles.len())
            };
            let children: Vec<_> = tiles
                .iter()
                .zip(rects.iter())
                .enumerate()
                .map(|(i, (tile, &rect))| {
                    self.render_tile(
                        ("zcall-group-tile", i),
                        tile,
                        rect,
                        is_conf && i == main_idx,
                        tiles.len() > 1,
                        cx,
                    )
                })
                .collect();
            return div()
                .absolute()
                .inset(px(0.))
                .bg(rgb(0x1a_1a_1a))
                .children(children)
                .into_any_element();
        }

        if let Some((_, remote_img)) = self.pictures.first() {
            if self.video_layout == VideoLayout::Split
                && (self.sharing || self.local_image.is_some() || self.camera_on)
            {
                let (left_r, right_r) = split_1v1_layout(vp_w, stage_h);
                let remote_tile = StageTile {
                    target: PinTarget::Remote(0),
                    name: self.display_peer_name().to_owned(),
                    avatar: self.peer.avatar.clone(),
                    blur: self.peer.blur.clone(),
                    status: String::new(),
                    image: Some(Arc::clone(remote_img)),
                    muted: self.peer_muted,
                    speaking: false,
                    contain: false,
                };
                let local_tile = StageTile {
                    target: PinTarget::Local,
                    name: if self.sharing {
                        "Bạn (Đang chia sẻ)".to_owned()
                    } else {
                        "Bạn".to_owned()
                    },
                    avatar: None,
                    blur: None,
                    status: if self.sharing || self.camera_on {
                        String::new()
                    } else {
                        "Đã tắt camera".to_owned()
                    },
                    image: self.local_image.clone(),
                    muted: self.muted,
                    speaking: false,
                    contain: self.sharing,
                };
                return div()
                    .absolute()
                    .inset(px(0.))
                    .bg(rgb(0x1a_1a_1a))
                    .child(self.render_tile(("zcall-split", 0), &local_tile, left_r, false, false, cx))
                    .child(self.render_tile(("zcall-split", 1), &remote_tile, right_r, false, false, cx))
                    .into_any_element();
            }
            return div()
                .absolute()
                .inset(px(0.))
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(0x00_00_00))
                .child(render_video_fit(Arc::clone(remote_img), vp_w, stage_h))
                .child(
                    div()
                        .absolute()
                        .left(px(0.))
                        .bottom(px(0.))
                        .h(px(34.))
                        .px(px(12.))
                        .bg(hsla(0., 0., 0., 0.75))
                        .flex()
                        .items_center()
                        .text_size(px(14.))
                        .text_color(rgb(0xff_ff_ff))
                        .child(self.display_peer_name().to_owned()),
                )
                .into_any_element();
        }

        if let Some(local) = self.local_image.clone() {
            if self.sharing {
                return div()
                    .absolute()
                    .inset(px(0.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(0x00_00_00))
                    .child(render_video_contain(local, vp_w, stage_h))
                    .into_any_element();
            }
            return div()
                .absolute()
                .inset(px(0.))
                .bg(rgb(0x00_00_00))
                .child(
                    img(local)
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover),
                )
                .child(div().absolute().inset(px(0.)).bg(hsla(0., 0., 0., 0.35)))
                .into_any_element();
        }

        div().into_any_element()
    }

    /// `#pip` from call.css: top-right, local camera or shared screen preview.
    /// Click toggles between Main Speaker (PiP) and 50/50 Grid (Split).
    fn local_pip(&self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let vw = f32::from(window.viewport_size().width);
        let (pip_w, pip_h) = if self.sharing {
            ((vw * 0.30).clamp(200., 340.), (vw * 0.17).clamp(114., 192.))
        } else {
            ((vw * 0.265625).clamp(170., 320.), (vw * 0.15).clamp(96., 180.))
        };
        let label = if self.sharing {
            "Bạn • Đang chia sẻ"
        } else {
            "Bạn"
        };
        div()
            .id("zcall-local-pip")
            .absolute()
            .top(px(8.))
            .right(px(8.))
            .w(px(pip_w))
            .h(px(pip_h))
            .rounded(px(6.))
            .border_1()
            .border_color(hsla(0., 0., 1., 0.22))
            .overflow_hidden()
            .bg(rgb(0x000000))
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_layout(cx);
            }))
            .flex()
            .items_center()
            .justify_center()
            .when_some(self.local_image.clone(), |el, image| {
                if self.sharing {
                    el.child(render_video_contain(image, pip_w, pip_h))
                } else {
                    el.child(
                        img(image)
                            .w(px(pip_w))
                            .h(px(pip_h))
                            .object_fit(gpui::ObjectFit::Cover),
                    )
                }
            })
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .bottom(px(0.))
                    .h(px(21.))
                    .px(px(6.))
                    .bg(hsla(0., 0., 0., 0.6))
                    .text_size(px(12.))
                    .line_height(px(21.))
                    .child(label),
            )
            .into_any_element()
    }

    fn avatar(&self, d: f32, ring: f32, ring_color: gpui::Hsla) -> impl IntoElement {
        avatar_circle(&self.peer.name, self.peer.avatar.clone(), d, ring, ring_color)
    }

    fn render_popup(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let video = self.video;
        let shadow = px(zalo_design::WINDOW_SHADOW);
        window.set_client_inset(shadow);
        window.set_background_appearance(WindowBackgroundAppearance::Transparent);
        // A click on a button must not start a window drag, or mouse-up never
        // lands on the button and answer / decline / close do nothing.
        let round = |id: &'static str, file: &'static str| {
            div()
                .id(id)
                .size(px(42.))
                .rounded(px(21.))
                .cursor_pointer()
                .hover(|s| s.opacity(0.9))
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(icon(file, 42.))
        };
        let blue = div()
            .id("incoming")
            .window_control_area(gpui::WindowControlArea::Drag)
            .occlude()
            .size_full()
            .relative()
            .rounded(px(zalo_design::WINDOW_RADIUS))
            .overflow_hidden()
            .bg(rgb(0x39_89_ff))
            .text_color(rgb(0xffffff))
            .font_family(crate::fonts::FAMILY_UI)
            .flex()
            .flex_col()
            .items_center()
            .shadow(vec![gpui::BoxShadow {
                color: hsla(0., 0., 0., 0.4),
                offset: gpui::point(px(0.), px(1.)),
                blur_radius: shadow / 2.,
                spread_radius: px(0.),
            }])
            .when(!cfg!(target_os = "windows"), |el| {
                el.on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            })
            .child(
                div()
                    .id("incoming-close")
                    .absolute()
                    .top(px(5.))
                    .right(px(5.))
                    .size(px(24.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(0., 0., 1., 0.15)))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, _| this.decline(window)))
                    .child(icon("close_white.png", 24.)),
            )
            .child(
                div()
                    .mt(px(23.))
                    .child(self.avatar(60., 0., hsla(0., 0., 0., 0.))),
            )
            .child(
                div()
                    .mt(px(4.))
                    .px(px(16.))
                    .max_w_full()
                    .text_size(px(19.))
                    .line_height(px(24.))
                    .font_family(FAMILY_SEMIBOLD)
                    .truncate()
                    .child(self.peer.name.clone()),
            )
            .child(
                div()
                    .mt(px(1.))
                    .text_size(px(16.))
                    .line_height(px(21.))
                    .child(if video {
                        "Zalo: Cuộc gọi video đến"
                    } else {
                        "Zalo: Cuộc gọi thoại đến"
                    }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(162.))
                    .left_0()
                    .right_0()
                    .flex()
                    .flex_row()
                    .justify_center()
                    .gap(px(42.))
                    .child(round("incoming-reject", "endcall.png").on_click(
                        cx.listener(|this, _: &ClickEvent, window, _| this.decline(window)),
                    ))
                    .child(
                        round(
                            "incoming-accept",
                            if video {
                                "accept_videocall.png"
                            } else {
                                "accept_audiocall.png"
                            },
                        )
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| this.accept(!video, window, cx),
                        )),
                    ),
            )
            .when(video, |el| {
                el.child(
                    div()
                        .id("incoming-nocam")
                        .absolute()
                        .left(px(22.))
                        .right(px(22.))
                        .bottom(px(20.))
                        .h(px(40.))
                        .rounded(px(4.))
                        .bg(rgb(0x2c_7f_e9))
                        .hover(|s| s.bg(rgb(0x27_74_d8)))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(8.))
                        .text_size(px(17.))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.accept(true, window, cx)
                        }))
                        .child(icon("camera_off1.png", 32.))
                        .child("Trả lời không mở camera"),
                )
            });
        div()
            .id("incoming-frame")
            .size_full()
            .bg(transparent_black())
            .p(shadow)
            .child(blue)
            .into_any_element()
    }

    fn render_call(&self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (state, connected, ended, notice, debug) = self.snapshot();
        let up = connected.is_some();
        let status = if ended {
            if notice.is_empty() {
                "Cuộc gọi đã kết thúc".to_owned()
            } else {
                notice
            }
        } else if up {
            String::new()
        } else if state == "failed" {
            if notice.is_empty() {
                "Không đổ chuông được tới thành viên".to_owned()
            } else {
                notice
            }
        } else if state == "ringing" {
            "Đang đổ chuông...".to_owned()
        } else if state == "incoming" {
            "Đang kết nối...".to_owned()
        } else {
            "Đang kết nối...".to_owned()
        };
        let timer = connected.filter(|_| !ended).map(|t| {
            let s = t.elapsed().as_secs();
            if s >= 3600 {
                format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
            } else {
                format!("{:02}:{:02}", s / 60, s % 60)
            }
        });
        let hint = self
            .hint
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(4))
            .map(|(t, _)| t.clone());
        let audio_err = (!ended
            && debug.starts_with("Không")
            && !(self.sharing && debug.to_lowercase().contains("camera")))
        .then_some(debug);
        let muted = self.muted;
        let has_pictures = !self.pictures.is_empty();
        let has_stage = self.group || has_pictures || (!self.group && self.local_image.is_some());
        let show_center_card = !self.group && !has_pictures;
        let bar = call_bar_state(self.video, self.group, up, has_pictures, self.sharing, ended);
        let fullscreen = window.is_fullscreen();
        let pill = || {
            div()
                .flex()
                .flex_row()
                .w(px(80.))
                .h(px(40.))
                .rounded(px(20.))
                .bg(rgb(0x1d_1d_1d))
                .border_1()
                .border_color(rgb(0x3a_3a_3a))
        };
        let pill_btn = |id: &'static str| {
            div()
                .id(id)
                .size(px(40.))
                .rounded(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(hsla(0., 0., 1., 0.12)))
        };
        let caret = |id: &'static str, disabled: bool| {
            pill_btn(id).child(icon(
                if disabled {
                    "dropupdisable.png"
                } else {
                    "dropup.png"
                },
                18.,
            ))
        };
        // Voice: avatar 125 with the grey ring while ringing, 160 with a white ring when up.
        let (d, ring, ring_color) = match (self.video, up) {
            (true, _) => (84., 0., hsla(0., 0., 0., 0.)),
            (false, true) => (160., 2., hsla(0., 0., 1., 1.)),
            (false, false) => (125., 6., hsla(0., 0., 0.59, 0.55)),
        };
        let mut peer_chips: Vec<&'static str> = Vec::new();
        if !self.group && up && !ended {
            if self.peer_muted {
                peer_chips.push("Đã tắt micro");
            }
            if self.peer_cam_off && self.video {
                peer_chips.push("Đã tắt camera");
            }
            if self.peer_sharing {
                peer_chips.push("Đang chia sẻ màn hình");
            }
        }
        div()
            .id("zcall")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                this.handle_key(e, window, cx);
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x000000))
            .text_color(rgb(0xffffff))
            .font_family(crate::fonts::FAMILY_UI)
            .child(
                div()
                    .id("zcall-main")
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .bg(rgb(0x14_12_10))
                    .on_click(cx.listener(|this, e: &ClickEvent, window, cx| {
                        if e.click_count() >= 2 && (this.video || this.group) {
                            window.toggle_fullscreen();
                            cx.notify();
                        }
                    }))
                    // `#bg`: the avatar blurred (a 6 px decode stretched) and darkened.
                    .when_some(self.peer.blur.clone(), |el, b| {
                        el.child(
                            img(b)
                                .absolute()
                                .inset(px(-60.))
                                .size_full()
                                .object_fit(gpui::ObjectFit::Cover),
                        )
                        .child(div().absolute().inset(px(0.)).bg(hsla(0., 0., 0., 0.68)))
                    })
                    .when(has_stage, |el| el.child(self.render_stage(window, cx)))
                    .when(
                        !self.group
                            && self.video_layout == VideoLayout::Pip
                            && self.local_image.is_some()
                            && has_pictures,
                        |el| el.child(self.local_pip(window, cx)),
                    )
                    .when(self.sharing && !ended, |el| {
                        let label = if self.share_label.is_empty() {
                            "Đang chia sẻ màn hình".to_owned()
                        } else {
                            format!("Đang chia sẻ: {}", self.share_label)
                        };
                        el.child(
                            div()
                                .absolute()
                                .top(px(8.))
                                .left_0()
                                .right_0()
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .px(px(12.))
                                        .py(px(5.))
                                        .rounded(px(16.))
                                        .bg(hsla(0., 0., 0., 0.78))
                                        .border_1()
                                        .border_color(hsla(0., 0., 1., 0.16))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(10.))
                                        .child(
                                            div()
                                                .size(px(8.))
                                                .rounded(px(4.))
                                                .bg(rgb(0x42_d5_8c)),
                                        )
                                        .child(
                                            div()
                                                .max_w(px(280.))
                                                .truncate()
                                                .text_size(px(12.))
                                                .text_color(rgb(0xff_ff_ff))
                                                .child(label),
                                        )
                                        .child(
                                            div()
                                                .id("zcall-share-stop-banner")
                                                .px(px(9.))
                                                .py(px(2.))
                                                .rounded(px(10.))
                                                .bg(rgb(0xd9_30_25))
                                                .hover(|s| s.bg(rgb(0xe5_39_35)))
                                                .cursor_pointer()
                                                .text_size(px(11.))
                                                .font_family(FAMILY_SEMIBOLD)
                                                .text_color(rgb(0xff_ff_ff))
                                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    this.send(UiAction::Screen { on: false });
                                                    cx.notify();
                                                }))
                                                .child("Dừng"),
                                        ),
                                ),
                        )
                    })
                    .when(has_pictures, |el| {
                        el.when_some(audio_err.clone(), |el, text| {
                            el.child(
                                div()
                                    .absolute()
                                    .top(px(36.))
                                    .left(px(16.))
                                    .right(px(16.))
                                    .flex()
                                    .justify_center()
                                    .child(
                                        div()
                                            .px(px(12.))
                                            .py(px(4.))
                                            .rounded(px(13.))
                                            .bg(hsla(0., 0.7, 0.4, 0.9))
                                            .text_size(px(13.))
                                            .child(text),
                                    ),
                            )
                        })
                    })
                    .when(!peer_chips.is_empty(), |el| {
                        el.child(
                            div()
                                .absolute()
                                .top(px(40.))
                                .left_0()
                                .right_0()
                                .flex()
                                .flex_row()
                                .justify_center()
                                .gap(px(8.))
                                .children(peer_chips.into_iter().map(|label| {
                                    div()
                                        .px(px(10.))
                                        .py(px(4.))
                                        .rounded(px(12.))
                                        .bg(hsla(0., 0., 0., 0.6))
                                        .flex()
                                        .items_center()
                                        .gap(px(5.))
                                        .text_size(px(12.))
                                        .when(label == "Đã tắt micro", |d| {
                                            d.child(icon("micoff.png", 13.))
                                        })
                                        .child(label)
                                })),
                        )
                    })
                    .when_some(timer, |el, t| {
                        if self.video || self.group {
                            el.child(
                                div()
                                    .absolute()
                                    .left(px(0.))
                                    .top(px(0.))
                                    .w(px(70.))
                                    .h(px(33.))
                                    .bg(hsla(0., 0., 0., 0.85))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(13.))
                                    .line_height(px(33.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(0x42_d5_8c))
                                    .child(t),
                            )
                        } else {
                            el.child(
                                div()
                                    .absolute()
                                    .left(px(17.))
                                    .top(px(8.))
                                    .text_size(px(13.))
                                    .line_height(px(18.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(0x42_d5_8c))
                                    .child(t),
                            )
                        }
                    })
                    .when(show_center_card, |el| {
                        el.child(
                            div()
                                .absolute()
                                .inset(px(0.))
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap(px(10.))
                                .child(self.avatar(d, ring, ring_color))
                                .when(!self.peer.name.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .mt(px(4.))
                                            .text_size(px(20.))
                                            .line_height(px(26.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(0xff_ff_ff))
                                            .child(self.peer.name.clone()),
                                    )
                                })
                                .child(
                                    div()
                                        .min_h(px(20.))
                                        .text_size(px(15.))
                                        .line_height(px(20.))
                                        .text_color(hsla(0., 0., 1., 0.85))
                                        .child(status.clone()),
                                )
                                .when_some(audio_err.clone(), |el, text| {
                                    el.child(
                                        div()
                                            .px(px(24.))
                                            .text_size(px(13.))
                                            .text_color(rgb(0xff_b4_b4))
                                            .child(text),
                                    )
                                })
                                .when(self.video && up && !ended, |el| {
                                    el.child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(hsla(0., 0., 1., 0.75))
                                            .child(if self.peer_cam_off {
                                                "Đối phương đang tắt camera"
                                            } else {
                                                "Đang chờ hình…"
                                            }),
                                    )
                                }),
                        )
                    })
                    .when_some(hint, |el, text| {
                        el.child(
                            div()
                                .absolute()
                                .top(px(10.))
                                .left_0()
                                .right_0()
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .px(px(12.))
                                        .py(px(4.))
                                        .rounded(px(13.))
                                        .bg(hsla(0., 0., 0., 0.7))
                                        .text_size(px(13.))
                                        .child(text),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h(px(50.))
                    .bg(rgb(0x0c_0c_0c))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap(px(20.))
                    .when(bar.show_screen, |el| {
                        el.child(
                            div()
                                .id("zcall-share")
                                .absolute()
                                .left(px(5.))
                                .top(px(5.))
                                .size(px(40.))
                                .rounded(px(8.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(hsla(0., 0., 1., 0.08)))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    if this.sharing {
                                        this.send(UiAction::Screen { on: false });
                                    } else if let Some(err) = crate::call_screen::grab_unavailable() {
                                        this.say(&err);
                                    } else {
                                        this.share_targets = crate::call_screen::list_share_targets();
                                        this.share_selected = 0;
                                        this.share_modal = true;
                                        this.menu = false;
                                    }
                                    cx.notify();
                                }))
                                .child(icon(
                                    if self.sharing {
                                        "sharescreenoff.png"
                                    } else {
                                        "sharescreen.png"
                                    },
                                    40.,
                                )),
                        )
                    })
                    .child(
                        pill()
                            .child(
                                pill_btn("zcall-cam")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        if !this.video {
                                            this.say("Cuộc gọi thoại không mở camera");
                                            cx.notify();
                                            return;
                                        }
                                        if this.sharing && !this.group {
                                            this.say("Camera đang tắt trong quá trình chia sẻ màn hình");
                                            cx.notify();
                                            return;
                                        }
                                        this.camera_on = !this.camera_on;
                                        let on = this.camera_on;
                                        this.send(UiAction::Camera { on });
                                        this.say(if on {
                                            "Đã bật camera"
                                        } else {
                                            "Đã tắt camera"
                                        });
                                        cx.notify();
                                    }))
                                    .child(icon(
                                        if bar.cam_disabled {
                                            "camera_disabled1.png"
                                        } else if self.camera_on {
                                            "camera1.png"
                                        } else {
                                            "camera_off1.png"
                                        },
                                        40.,
                                    )),
                            )
                            .child(
                                caret("zcall-cam-caret", bar.cam_disabled).on_click(cx.listener(
                                    |this, _: &ClickEvent, _, cx| {
                                        if !this.video {
                                            this.say("Cuộc gọi thoại không mở camera");
                                            cx.notify();
                                            return;
                                        }
                                        this.toggle_menu(cx);
                                    },
                                )),
                            ),
                    )
                    .child(
                        div()
                            .id("zcall-end")
                            .size(px(40.))
                            .rounded(px(20.))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.88))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, _| {
                                this.send(UiAction::Hangup);
                                this.handed_off = true;
                                this.play_kind("end", false);
                                this.release_pictures();
                                window.remove_window();
                            }))
                            .child(icon("endcall.png", 40.)),
                    )
                    .child(
                        pill()
                            .child(
                                pill_btn("zcall-mic")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.muted = !this.muted;
                                        let on = this.muted;
                                        this.send(UiAction::Mute { on });
                                        this.say(if on {
                                            "Đã tắt micro"
                                        } else {
                                            "Đã bật micro"
                                        });
                                        cx.notify();
                                    }))
                                    .child(icon(
                                        if muted { "mic_off1.png" } else { "mic1.png" },
                                        40.,
                                    )),
                            )
                            .child(caret("zcall-mic-caret", false).on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    this.toggle_menu(cx);
                                },
                            ))),
                    )
                    .when(bar.show_full, |el| {
                        let right_x = if bar.show_layout { 95. } else { 50. };
                        el.child(
                            div()
                                .id("zcall-full")
                                .absolute()
                                .right(px(right_x))
                                .top(px(5.))
                                .size(px(40.))
                                .rounded(px(8.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(hsla(0., 0., 1., 0.08)))
                                .on_click(cx.listener(|_, _: &ClickEvent, window, cx| {
                                    window.toggle_fullscreen();
                                    cx.notify();
                                }))
                                .child(icon(
                                    if fullscreen {
                                        "icn-call-minimizewindow@3x.png"
                                    } else {
                                        "icn-call-expandwindow@3x.png"
                                    },
                                    22.,
                                )),
                        )
                    })
                    .when(bar.show_layout, |el| {
                        // In Main Speaker mode (Pip for 1:1, Conference for group),
                        // show the 4-square Grid icon to switch into Grid/Split;
                        // in Grid/Split mode, show the Speaker icon to switch back.
                        let is_grid_btn = matches!(
                            self.video_layout,
                            VideoLayout::Conference | VideoLayout::Pip
                        );
                        el.child(
                            div()
                                .id("zcall-layout")
                                .absolute()
                                .right(px(50.))
                                .top(px(5.))
                                .size(px(40.))
                                .rounded(px(8.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(hsla(0., 0., 1., 0.08)))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.toggle_layout(cx);
                                }))
                                .child(icon(
                                    if is_grid_btn {
                                        "grid.png"
                                    } else {
                                        "conference.png"
                                    },
                                    28.,
                                )),
                        )
                    })
                    .child(
                        div()
                            .id("zcall-settings")
                            .absolute()
                            .right(px(5.))
                            .top(px(5.))
                            .size(px(40.))
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(hsla(0., 0., 1., 0.08)))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.toggle_device_status(cx);
                            }))
                            .child(icon("setting.png", 24.)),
                    ),
            )
            .when(self.menu, |el| {
                el.child(
                    div()
                        .id("zcall-menu-dismiss")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.menu = false;
                            cx.notify();
                        })),
                )
                .child(self.device_menu(cx))
            })
            .when(self.device_status_open, |el| {
                el.child(self.device_status_modal(cx))
            })
            .when(self.share_modal, |el| el.child(self.share_picker_modal(cx)))
            .into_any_element()
    }

    /// Screen / window share picker modal and "Start Sharing" confirmation banner
    /// matching the ZaloCall UI on macOS & Windows.
    fn share_picker_modal(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected = self
            .share_selected
            .min(self.share_targets.len().saturating_sub(1));
        let subtitle = if self.group {
            "Chọn màn hình, vùng hoặc cửa sổ bạn muốn trình chiếu cho nhóm"
        } else {
            "Camera của bạn sẽ tắt trong quá trình chia sẻ màn hình"
        };
        let rows: Vec<gpui::AnyElement> = self
            .share_targets
            .iter()
            .enumerate()
            .map(|(idx, target)| {
                let is_sel = idx == selected;
                let label = target.label();
                let detail = target.detail();
                div()
                    .id(("zcall-share-target", idx))
                    .w_full()
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if is_sel {
                        rgb(0x00_68_ff)
                    } else {
                        rgb(0xe5_e7_eb)
                    })
                    .bg(if is_sel {
                        rgb(0xef_f6_ff)
                    } else {
                        rgb(0xff_ff_ff)
                    })
                    .hover(|s| {
                        s.bg(if is_sel {
                            rgb(0xe5_f0_ff)
                        } else {
                            rgb(0xf5_f7_fa)
                        })
                    })
                    .cursor_pointer()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.share_selected = idx;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(0x1a_1d_1f))
                                    .child(label),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(rgb(0x6b_72_80))
                                    .child(detail),
                            ),
                    )
                    .when(is_sel, |d| {
                        d.child(
                            div()
                                .size(px(18.))
                                .rounded(px(9.))
                                .bg(rgb(0x00_68_ff))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(11.))
                                .font_family(crate::FAMILY_BOLD)
                                .text_color(rgb(0xff_ff_ff))
                                .child("✓"),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        div()
            .id("zcall-share-modal-backdrop")
            .absolute()
            .inset_0()
            .bg(hsla(0., 0., 0., 0.58))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.share_modal = false;
                cx.notify();
            }))
            .child(
                div()
                    .id("zcall-share-modal-card")
                    .w(px(430.))
                    .max_h(px(450.))
                    .rounded(px(10.))
                    .bg(rgb(0xff_ff_ff))
                    .text_color(rgb(0x1a_1d_1f))
                    .shadow_lg()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .w_full()
                            .py(px(16.))
                            .px(px(20.))
                            .bg(rgb(0xf2_f7_ff))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .w(px(128.))
                                    .h(px(74.))
                                    .rounded(px(8.))
                                    .bg(rgb(0xff_ff_ff))
                                    .border(px(2.))
                                    .border_color(rgb(0xd0_e2_ff))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .size(px(36.))
                                            .rounded(px(18.))
                                            .bg(rgb(0x00_68_ff))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(icon("sharescreen.png", 26.)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .px(px(20.))
                            .pt(px(14.))
                            .pb(px(10.))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .font_family(crate::FAMILY_BOLD)
                                    .text_color(rgb(0x1a_1d_1f))
                                    .child("Thông báo"),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(0x4b_55_63))
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        div()
                            .px(px(20.))
                            .pb(px(6.))
                            .text_size(px(12.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(0x5a_69_81))
                            .child("Chọn vùng hoặc cửa sổ chia sẻ"),
                    )
                    .child(
                        div()
                            .id("zcall-share-target-list")
                            .px(px(20.))
                            .max_h(px(176.))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .children(rows),
                    )
                    .child(
                        div()
                            .px(px(20.))
                            .py(px(16.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_end()
                            .gap(px(10.))
                            .child(
                                div()
                                    .id("zcall-share-cancel")
                                    .h(px(36.))
                                    .px(px(20.))
                                    .rounded(px(6.))
                                    .bg(rgb(0xea_ed_f0))
                                    .hover(|s| s.bg(rgb(0xdf_e3_e8)))
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(14.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(0x1a_1d_1f))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.share_modal = false;
                                        cx.notify();
                                    }))
                                    .child("Hủy"),
                            )
                            .child(
                                div()
                                    .id("zcall-share-confirm")
                                    .h(px(36.))
                                    .px(px(20.))
                                    .rounded(px(6.))
                                    .bg(rgb(0x00_68_ff))
                                    .hover(|s| s.bg(rgb(0x00_58_db)))
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(14.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(0xff_ff_ff))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        let target = this
                                            .share_targets
                                            .get(this.share_selected)
                                            .cloned()
                                            .unwrap_or_default();
                                        let label = target.label();
                                        if let Ok(mut s) = this.shared.lock() {
                                            s.share_target = target;
                                            s.share_label = label.clone();
                                        }
                                        this.share_label = label;
                                        this.share_modal = false;
                                        this.send(UiAction::Screen { on: true });
                                        cx.notify();
                                    }))
                                    .child("Bắt đầu trình chiếu"),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// "Device Status" modal opened from the in-call gear icon,
    /// matching the macOS ZaloCall UI (Microphone + 16-dot VU meter, Speaker/Headphones +
    /// "Test speaker/headphones" button + Volume slider, and Camera during video calls).
    fn device_status_modal(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active_mic_label = if self.audio.mic.is_empty() {
            "Thiết bị mặc định".to_owned()
        } else {
            self.audio
                .mics
                .iter()
                .find(|(id, _)| *id == self.audio.mic)
                .map(|(_, desc)| desc.clone())
                .unwrap_or_else(|| self.audio.mic.clone())
        };
        let active_spk_label = if self.audio.speaker.is_empty() {
            "Thiết bị mặc định".to_owned()
        } else {
            self.audio
                .speakers
                .iter()
                .find(|(id, _)| *id == self.audio.speaker)
                .map(|(_, desc)| desc.clone())
                .unwrap_or_else(|| self.audio.speaker.clone())
        };
        let selected_cam = resolve_camera_selection(&self.audio.camera);
        let active_cam_label = if selected_cam.is_none() {
            "Thiết bị mặc định".to_owned()
        } else {
            self.audio
                .cameras
                .iter()
                .find(|label| resolve_camera_selection(label) == selected_cam)
                .cloned()
                .unwrap_or_else(|| self.audio.camera.clone())
        };

        let status_banner = |ok: bool, icon_name: &'static str, text: &'static str| {
            let (bg_c, border_c, text_c, badge_bg, badge_sym) = if ok {
                (0xec_fd_f3, 0x9c_e6_bc, 0x0f_51_32, 0x16_a3_4a, "✓")
            } else {
                (0xfe_f2_f2, 0xfc_a5_a5, 0x99_1b_1b, 0xdc_26_26, "!")
            };
            div()
                .w_full()
                .px(px(12.))
                .py(px(9.))
                .rounded(px(6.))
                .bg(rgb(bg_c))
                .border_1()
                .border_color(rgb(border_c))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .child(icon(icon_name, 16.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(text_c))
                                .child(text),
                        ),
                )
                .child(
                    div()
                        .size(px(18.))
                        .rounded(px(9.))
                        .bg(rgb(badge_bg))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(0xff_ff_ff))
                        .child(badge_sym),
                )
        };

        let drop_option = |eid: String,
                           device: String,
                           label: String,
                           on: bool,
                           kind: &'static str| {
            div()
                .id(gpui::SharedString::from(eid))
                .h(px(32.))
                .px(px(10.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .bg(if on { rgb(0xee_f5_ff) } else { rgb(0xff_ff_ff) })
                .hover(|s| s.bg(rgb(0xf2_f6_fb)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.save_device(kind, &device);
                    this.send(UiAction::Device {
                        kind: kind.into(),
                        id: device.clone(),
                    });
                    this.ds_dropdown = None;
                    this.say(match kind {
                        "mic" => "Đã đổi micro",
                        "camera" => "Đã đổi camera",
                        _ => "Đã đổi loa/tai nghe",
                    });
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(14.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(on, |el| el.child(icon("icn_callsetting_check.png", 12.))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(13.))
                        .text_color(rgb(0x08_1c_36))
                        .child(label),
                )
                .into_any_element()
        };

        let mut mic_opts = vec![drop_option(
            "ds-mic-default".into(),
            String::new(),
            "Thiết bị mặc định".into(),
            self.audio.mic.is_empty(),
            "mic",
        )];
        for (name, desc) in &self.audio.mics {
            mic_opts.push(drop_option(
                format!("ds-mic-{}", device_row_id(name)),
                name.clone(),
                desc.clone(),
                self.audio.mic == *name,
                "mic",
            ));
        }

        let mut spk_opts = vec![drop_option(
            "ds-spk-default".into(),
            String::new(),
            "Thiết bị mặc định".into(),
            self.audio.speaker.is_empty(),
            "speaker",
        )];
        for (name, desc) in &self.audio.speakers {
            spk_opts.push(drop_option(
                format!("ds-spk-{}", device_row_id(name)),
                name.clone(),
                desc.clone(),
                self.audio.speaker == *name,
                "speaker",
            ));
        }

        let mut cam_opts = Vec::new();
        if self.video || self.camera_on {
            cam_opts.push(drop_option(
                "ds-cam-default".into(),
                String::new(),
                "Thiết bị mặc định".into(),
                selected_cam.is_none(),
                "camera",
            ));
            for label in &self.audio.cameras {
                let path = resolve_camera_selection(label);
                let on = path.is_some() && path == selected_cam;
                cam_opts.push(drop_option(
                    format!("ds-cam-{}", device_row_id(label)),
                    label.clone(),
                    label.clone(),
                    on,
                    "camera",
                ));
            }
        }

        // 16-dot live microphone level meter matching macOS ZaloCall.
        let active_dots = usize::from(mic_level_to_dots(self.live_mic_level, self.muted));
        let mut dots = Vec::with_capacity(16);
        for idx in 0..16 {
            let lit = idx < active_dots;
            dots.push(
                div()
                    .size(px(9.))
                    .rounded(px(5.))
                    .bg(if lit {
                        rgb(0x2b_85_ff)
                    } else {
                        rgb(0xe2_e6_ec)
                    }),
            );
        }

        // Speaker test pill progress (0.0 ..= 1.0 over 1.6 s).
        let testing = self.test_tone_at.is_some();
        let test_frac = self
            .test_tone_at
            .map(|at| (at.elapsed().as_secs_f32() / 1.6).clamp(0.08, 1.0))
            .unwrap_or(0.0);

        // Interactive speaker volume slider (0..=100% in 5% steps).
        let spk_pct = self.speaker_volume;
        let spk_frac = f32::from(spk_pct) / 100.0;
        let mut spk_hits = Vec::with_capacity(21);
        for step in 0u8..=20 {
            let value = step * 5;
            spk_hits.push(
                div()
                    .id(gpui::SharedString::from(format!("ds-spk-vol-{value}")))
                    .flex_1()
                    .h_full()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.spk_drag = true;
                            this.commit_speaker_volume(value, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, e: &gpui::MouseMoveEvent, _, cx| {
                        if this.spk_drag && e.dragging() {
                            this.commit_speaker_volume(value, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                            cx.stop_propagation();
                            this.spk_drag = false;
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
            );
        }

        let aec_on = self.audio.call_aec;
        let ns_on = self.audio.call_ns;
        let dsp_row = |eid: &'static str, label: &'static str, on: bool, toggle_aec: bool| {
            div()
                .id(eid)
                .w_full()
                .py(px(4.))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.toggle_audio_dsp(toggle_aec, cx);
                }))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(0x08_1c_36))
                        .child(label),
                )
                .child(
                    div()
                        .w(px(34.))
                        .h(px(18.))
                        .p(px(2.))
                        .rounded(px(9.))
                        .bg(if on { rgb(0x00_68_ff) } else { rgb(0xc8_cf_d8) })
                        .flex()
                        .items_center()
                        .when(on, |el| el.justify_end())
                        .child(div().size(px(14.)).rounded(px(7.)).bg(rgb(0xff_ff_ff))),
                )
        };

        let mic_open = self.ds_dropdown == Some("mic");
        let spk_open = self.ds_dropdown == Some("speaker");
        let cam_open = self.ds_dropdown == Some("camera");

        div()
            .id("zcall-device-status-backdrop")
            .absolute()
            .inset_0()
            .bg(hsla(0., 0., 0., 0.42))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.device_status_open = false;
                this.ds_dropdown = None;
                this.spk_drag = false;
                cx.notify();
            }))
            .child(
                div()
                    .id("zcall-device-status-card")
                    .w(px(364.))
                    .max_h(px(490.))
                    .rounded(px(10.))
                    .bg(rgb(0xff_ff_ff))
                    .border_1()
                    .border_color(rgb(0xdc_e0_e5))
                    .text_color(rgb(0x08_1c_36))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if this.ds_dropdown.is_some() {
                            this.ds_dropdown = None;
                            cx.notify();
                        }
                    }))
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                            if this.spk_drag {
                                this.spk_drag = false;
                                cx.notify();
                            }
                        }),
                    )
                    // Header
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .h(px(42.))
                            .px(px(14.))
                            .border_b_1()
                            .border_color(rgb(0xeb_ee_f2))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(0x08_1c_36))
                                    .child("Tình trạng thiết bị"),
                            )
                            .child(
                                div()
                                    .id("zcall-ds-close")
                                    .absolute()
                                    .right(px(8.))
                                    .top(px(8.))
                                    .size(px(26.))
                                    .rounded(px(13.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(0xf0_f2_f5)))
                                    .text_size(px(16.))
                                    .text_color(rgb(0x5a_69_81))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.device_status_open = false;
                                        this.ds_dropdown = None;
                                        cx.notify();
                                    }))
                                    .child("×"),
                            ),
                    )
                    // Scrollable body
                    .child(
                        div()
                            .id("zcall-ds-body")
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .px(px(16.))
                            .py(px(14.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            // --- Section 1: Micro ---
                            .child(status_banner(
                                !self.muted,
                                "voice.png",
                                if self.muted {
                                    "Micro đang tắt tiếng"
                                } else {
                                    "Micro đang hoạt động"
                                },
                            ))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(0x6c_7a_89))
                                    .child("Micro"),
                            )
                            .child(
                                div()
                                    .id("zcall-ds-mic-drop")
                                    .h(px(36.))
                                    .px(px(11.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(if mic_open {
                                        rgb(0x00_68_ff)
                                    } else {
                                        rgb(0xd1_d7_de)
                                    })
                                    .bg(rgb(0xff_ff_ff))
                                    .cursor_pointer()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.ds_dropdown = if this.ds_dropdown == Some("mic") {
                                            None
                                        } else {
                                            Some("mic")
                                        };
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(px(13.))
                                            .text_color(rgb(0x08_1c_36))
                                            .child(active_mic_label),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(0x5a_69_81))
                                            .child(if mic_open { "▴" } else { "▾" }),
                                    ),
                            )
                            .when(mic_open, |el| {
                                el.child(
                                    div()
                                        .rounded(px(6.))
                                        .border_1()
                                        .border_color(rgb(0xd1_d7_de))
                                        .bg(rgb(0xff_ff_ff))
                                        .overflow_hidden()
                                        .flex()
                                        .flex_col()
                                        .children(mic_opts),
                                )
                            })
                            // 16-dot live mic level meter
                            .child(
                                div()
                                    .w_full()
                                    .px(px(10.))
                                    .py(px(10.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(rgb(0xeb_ee_f2))
                                    .bg(rgb(0xfa_fb_fc))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .children(dots),
                            )
                            // Live DSP toggles: AEC3 + RNNoise AI noise suppression
                            .child(dsp_row("zcall-ds-aec", "Khử vọng âm (AEC)", aec_on, true))
                            .child(dsp_row("zcall-ds-ns", "Lọc tiếng ồn nền (AI)", ns_on, false))
                            // Divider
                            .child(div().my(px(4.)).h(px(1.)).bg(rgb(0xeb_ee_f2)))
                            // --- Section 2: Loa/tai nghe ---
                            .child(status_banner(
                                !self.speaker_off,
                                "speak.png",
                                if self.speaker_off {
                                    "Loa/tai nghe đang tắt"
                                } else {
                                    "Loa/tai nghe đang hoạt động"
                                },
                            ))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(0x6c_7a_89))
                                    .child("Loa/tai nghe"),
                            )
                            .child(
                                div()
                                    .id("zcall-ds-spk-drop")
                                    .h(px(36.))
                                    .px(px(11.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(if spk_open {
                                        rgb(0x00_68_ff)
                                    } else {
                                        rgb(0xd1_d7_de)
                                    })
                                    .bg(rgb(0xff_ff_ff))
                                    .cursor_pointer()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.ds_dropdown = if this.ds_dropdown == Some("speaker") {
                                            None
                                        } else {
                                            Some("speaker")
                                        };
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(px(13.))
                                            .text_color(rgb(0x08_1c_36))
                                            .child(active_spk_label),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(0x5a_69_81))
                                            .child(if spk_open { "▴" } else { "▾" }),
                                    ),
                            )
                            .when(spk_open, |el| {
                                el.child(
                                    div()
                                        .rounded(px(6.))
                                        .border_1()
                                        .border_color(rgb(0xd1_d7_de))
                                        .bg(rgb(0xff_ff_ff))
                                        .overflow_hidden()
                                        .flex()
                                        .flex_col()
                                        .children(spk_opts),
                                )
                            })
                            // Speaker test row
                            .child(
                                div()
                                    .pt(px(2.))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(rgb(0x08_1c_36))
                                            .child("Kiểm tra loa/tai nghe"),
                                    )
                                    .child(
                                        div()
                                            .id("zcall-ds-test-spk")
                                            .h(px(30.))
                                            .w(px(134.))
                                            .px(px(6.))
                                            .rounded(px(15.))
                                            .bg(rgb(0xe8_f1_ff))
                                            .hover(|s| s.bg(rgb(0xd8_e8_ff)))
                                            .cursor_pointer()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap(px(8.))
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, _, cx| {
                                                    cx.stop_propagation();
                                                    this.trigger_speaker_test(cx);
                                                },
                                            ))
                                            .child(
                                                div()
                                                    .size(px(20.))
                                                    .rounded(px(10.))
                                                    .bg(rgb(0xff_ff_ff))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .text_size(px(10.))
                                                    .text_color(rgb(0x00_68_ff))
                                                    .child(if testing { "♪" } else { "▶" }),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .h(px(3.))
                                                    .rounded(px(2.))
                                                    .bg(rgb(0xbc_d6_fa))
                                                    .overflow_hidden()
                                                    .child(
                                                        div()
                                                            .h_full()
                                                            .w(gpui::relative(if testing {
                                                                test_frac
                                                            } else {
                                                                0.0
                                                            }))
                                                            .bg(rgb(0x00_68_ff)),
                                                    ),
                                            ),
                                    ),
                            )
                            // Speaker volume label + interactive slider
                            .child(
                                div()
                                    .pt(px(2.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .justify_between()
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(0x6c_7a_89))
                                                    .child("Âm lượng"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .font_family(FAMILY_SEMIBOLD)
                                                    .text_color(rgb(0x08_1c_36))
                                                    .child(format!("{spk_pct}%")),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("zcall-ds-spk-slider")
                                            .relative()
                                            .w_full()
                                            .h(px(22.))
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .child(
                                                div()
                                                    .w_full()
                                                    .h(px(4.))
                                                    .rounded(px(2.))
                                                    .bg(rgb(0xdf_e4_ea))
                                                    .overflow_hidden()
                                                    .child(
                                                        div()
                                                            .h_full()
                                                            .w(gpui::relative(spk_frac))
                                                            .bg(rgb(0x2b_85_ff)),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .absolute()
                                                    .top(px(4.))
                                                    .left(gpui::relative(
                                                        (spk_frac * 0.94).clamp(0.0, 0.94),
                                                    ))
                                                    .size(px(14.))
                                                    .rounded(px(7.))
                                                    .bg(rgb(0xff_ff_ff))
                                                    .border(px(2.))
                                                    .border_color(rgb(0x2b_85_ff)),
                                            )
                                            .child(
                                                div()
                                                    .absolute()
                                                    .inset_0()
                                                    .flex()
                                                    .flex_row()
                                                    .children(spk_hits),
                                            ),
                                    ),
                            )
                            // Optional Camera section in video calls
                            .when(!cam_opts.is_empty(), |el| {
                                el.child(div().my(px(4.)).h(px(1.)).bg(rgb(0xeb_ee_f2)))
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(0x6c_7a_89))
                                            .child("Camera"),
                                    )
                                    .child(
                                        div()
                                            .id("zcall-ds-cam-drop")
                                            .h(px(36.))
                                            .px(px(11.))
                                            .rounded(px(6.))
                                            .border_1()
                                            .border_color(if cam_open {
                                                rgb(0x00_68_ff)
                                            } else {
                                                rgb(0xd1_d7_de)
                                            })
                                            .bg(rgb(0xff_ff_ff))
                                            .cursor_pointer()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(8.))
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, _, cx| {
                                                    cx.stop_propagation();
                                                    this.ds_dropdown =
                                                        if this.ds_dropdown == Some("camera") {
                                                            None
                                                        } else {
                                                            Some("camera")
                                                        };
                                                    cx.notify();
                                                },
                                            ))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .truncate()
                                                    .text_size(px(13.))
                                                    .text_color(rgb(0x08_1c_36))
                                                    .child(active_cam_label),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(0x5a_69_81))
                                                    .child(if cam_open { "▴" } else { "▾" }),
                                            ),
                                    )
                                    .when(cam_open, |d| {
                                        d.child(
                                            div()
                                                .rounded(px(6.))
                                                .border_1()
                                                .border_color(rgb(0xd1_d7_de))
                                                .bg(rgb(0xff_ff_ff))
                                                .overflow_hidden()
                                                .flex()
                                                .flex_col()
                                                .children(cam_opts),
                                        )
                                    })
                            }),
                    )
                    // Footer
                    .child(
                        div()
                            .flex_none()
                            .px(px(16.))
                            .py(px(10.))
                            .border_t_1()
                            .border_color(rgb(0xeb_ee_f2))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .child(
                                div()
                                    .id("zcall-ds-cancel")
                                    .h(px(32.))
                                    .px(px(18.))
                                    .rounded(px(6.))
                                    .bg(rgb(0xea_ed_f0))
                                    .hover(|s| s.bg(rgb(0xdf_e3_e8)))
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(0x08_1c_36))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.device_status_open = false;
                                        this.ds_dropdown = None;
                                        cx.notify();
                                    }))
                                    .child("Hủy"),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// White device menu above the bar: microphone, speaker mute, speakers.
    fn device_menu(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let section = |title: &'static str, icon_name: &'static str, rows: Vec<gpui::AnyElement>| {
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .px(px(12.))
                        .h(px(28.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(0x5a_69_81))
                        .child(icon(icon_name, 14.))
                        .child(title),
                )
                .children(rows)
        };
        let row = |eid: String, device: String, label: String, on: bool, kind: &'static str| {
            div()
                .id(gpui::SharedString::from(eid))
                .h(px(30.))
                .px(px(12.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(0xf0_f4_f8)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.save_device(kind, &device);
                    this.send(UiAction::Device {
                        kind: kind.into(),
                        id: device.clone(),
                    });
                    this.menu = false;
                    this.say(match kind {
                        "mic" => "Đã đổi micro",
                        "camera" => "Đã đổi camera",
                        _ => "Đã đổi loa",
                    });
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(14.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(on, |el| el.child(icon("icn_callsetting_check.png", 12.))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(14.))
                        .child(label),
                )
                .into_any_element()
        };
        let mut mics = vec![row(
            "mic-default".into(),
            String::new(),
            "Mặc định".into(),
            self.audio.mic.is_empty(),
            "mic",
        )];
        for (name, desc) in &self.audio.mics {
            mics.push(row(
                format!("mic-{}", device_row_id(name)),
                name.clone(),
                desc.clone(),
                self.audio.mic == *name,
                "mic",
            ));
        }
        let mut speakers = vec![row(
            "spk-default".into(),
            String::new(),
            "Mặc định".into(),
            self.audio.speaker.is_empty(),
            "speaker",
        )];
        for (name, desc) in &self.audio.speakers {
            speakers.push(row(
                format!("spk-{}", device_row_id(name)),
                name.clone(),
                desc.clone(),
                self.audio.speaker == *name,
                "speaker",
            ));
        }
        let selected_cam = resolve_camera_selection(&self.audio.camera);
        let mut cameras = Vec::new();
        if self.video {
            cameras.push(row(
                "cam-default".into(),
                String::new(),
                "Mặc định".into(),
                selected_cam.is_none(),
                "camera",
            ));
            for label in &self.audio.cameras {
                let path = resolve_camera_selection(label);
                let on = path.is_some() && path == selected_cam;
                cameras.push(row(
                    format!("cam-{}", device_row_id(label)),
                    label.clone(),
                    label.clone(),
                    on,
                    "camera",
                ));
            }
        }
        let speaker_off = self.speaker_off;
        div()
            .id("zcall-devices")
            .absolute()
            .bottom(px(58.))
            .right(px(8.))
            .w(px(280.))
            .max_h(px(400.))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.mic_drag {
                        this.mic_drag = false;
                        cx.notify();
                    }
                }),
            )
            .rounded(px(6.))
            .bg(rgb(0xff_ff_ff))
            .text_color(rgb(0x1a_1d_1f))
            .shadow_lg()
            .overflow_y_scroll()
            .py(px(6.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(!cameras.is_empty(), |el| {
                el.child(section("Chọn camera", "video.png", cameras))
                    .child(div().mx(px(12.)).h(px(1.)).bg(rgb(0xe6_e8_eb)))
            })
            .child(section("Chọn micro", "voice.png", mics))
            .child(self.mic_volume_block(cx))
            .child(div().mx(px(12.)).h(px(1.)).bg(rgb(0xe6_e8_eb)))
            .child(
                div()
                    .id("zcall-speaker-toggle")
                    .h(px(30.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(0xf0_f4_f8)))
                    .text_size(px(14.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.speaker_off = !this.speaker_off;
                        let off = this.speaker_off;
                        this.send(UiAction::Speaker { off });
                        this.say(if off {
                            "Đã tắt loa"
                        } else {
                            "Đã bật loa"
                        });
                        cx.notify();
                    }))
                    .child(if speaker_off {
                        "Bật loa"
                    } else {
                        "Tắt loa"
                    }),
            )
            .child(section("Chọn loa", "speak.png", speakers))
            .into_any_element()
    }

    /// "Microphone volume" under the mic list. Dragging does not close the menu.
    /// 100% is Pulse 0 dB. The steps are 5% so a short track stays easy to hit.
    fn mic_volume_block(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let pct = self.audio.mic_volume;
        let dec = pct.saturating_sub(5);
        let inc = pct.saturating_add(5).min(100);
        let frac = f32::from(pct) / 100.0;
        let mut hits = Vec::new();
        for step in 0u8..=20 {
            let value = step * 5;
            hits.push(
                div()
                    .id(gpui::SharedString::from(format!("mic-hit-{value}")))
                    .flex_1()
                    .h_full()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.mic_drag = true;
                            this.commit_mic_volume(value, true, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, e: &gpui::MouseMoveEvent, _, cx| {
                        if this.mic_drag && e.dragging() {
                            this.commit_mic_volume(value, false, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                            cx.stop_propagation();
                            this.mic_drag = false;
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
            );
        }
        let step_btn = |id: &'static str, label: &'static str, next: u8| {
            div()
                .id(id)
                .w(px(22.))
                .h(px(22.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(rgb(0xf0_f4_f8)))
                .text_size(px(16.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.commit_mic_volume(next, true, cx);
                }))
                .child(label)
                .into_any_element()
        };
        div()
            .id("zcall-mic-volume")
            .px(px(12.))
            .pt(px(2.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .whitespace_nowrap()
                            .text_size(px(12.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(0x5a_69_81))
                            .child("Âm lượng micro"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(2.))
                            .children([
                                step_btn("mic-vol-dec", "−", dec),
                                div()
                                    .w(px(40.))
                                    .flex()
                                    .justify_center()
                                    .text_size(px(12.))
                                    .text_color(rgb(0x1a_1d_1f))
                                    .child(format!("{pct}%"))
                                    .into_any_element(),
                                step_btn("mic-vol-inc", "+", inc),
                            ]),
                    ),
            )
            .child(
                div()
                    .relative()
                    .h(px(22.))
                    .w_full()
                    .cursor_pointer()
                    .child(
                        div()
                            .absolute()
                            .top(px(9.))
                            .left_0()
                            .right_0()
                            .h(px(4.))
                            .rounded(px(2.))
                            .bg(rgb(0xe6_e8_eb)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(9.))
                            .left_0()
                            .h(px(4.))
                            .w(gpui::relative(frac))
                            .rounded(px(2.))
                            .bg(rgb(0x00_68_ff)),
                    )
                    .child(div().absolute().inset_0().flex().flex_row().children(hits)),
            )
            .into_any_element()
    }

    /// Persists `micVolume` and tells the call worker. `force` locks the clip
    /// guard even when the percent did not change (a click on the current step).
    fn commit_mic_volume(&mut self, percent: u8, force: bool, cx: &mut Context<Self>) {
        let percent = percent.min(100);
        let changed = self.audio.mic_volume != percent;
        if !changed && !force {
            return;
        }
        self.audio.mic_volume = percent;
        self.send(UiAction::MicVolume {
            percent: i64::from(percent),
        });
        if changed {
            if let Some(dir) = self.audio.dir.clone() {
                let mut prefs = crate::prefs::Prefs::load(Some(dir));
                prefs.set("micVolume", serde_json::json!(percent));
            }
            cx.notify();
        }
    }
}

impl gpui::Render for CallView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.popup {
            self.render_popup(window, cx)
        } else {
            self.render_call(window, cx)
        }
    }
}

struct Job {
    video: bool,
    peer_id: String,
    peer_name: String,
    incoming: bool,
    control: Option<Value>,
    /// Outgoing or incoming group call.
    group: bool,
    /// Invited members for an outgoing group call (`{id, name}`).
    partners: Vec<Value>,
    /// `groupInfo.maxUsers`.
    max_users: u32,
    /// Group avatar URL.
    avatar: String,
}

fn one_to_one_job(
    video: bool,
    peer_id: String,
    peer_name: String,
    incoming: bool,
    control: Option<Value>,
) -> Job {
    Job {
        video,
        peer_id,
        peer_name,
        incoming,
        control,
        group: false,
        partners: Vec::new(),
        max_users: 0,
        avatar: String::new(),
    }
}

struct CallUi {
    shared: Arc<Mutex<Shared>>,
}

impl Ui for CallUi {
    fn status(&mut self, text: &str) {
        if !text.is_empty() {
            let owned = text.to_owned();
            set(&self.shared, |s| s.notice = owned);
        }
    }

    fn status_at(&mut self, text: &str, since: u64) {
        let owned = text.to_owned();
        set(&self.shared, |s| {
            if since > 0 && s.connected_at.is_none() {
                s.connected_at = Some(Instant::now());
            }
            if !owned.is_empty() {
                s.notice = owned;
            }
        });
    }

    fn rename(&mut self, name: &str) {
        if !name.is_empty() {
            let owned = name.to_owned();
            set(&self.shared, |s| s.peer_name_override = Some(owned));
        }
    }

    fn peer(&mut self, peer: &Value) {
        let cam_off = peer.get("camOff").and_then(Value::as_bool).unwrap_or(false);
        let muted = peer.get("muted").and_then(Value::as_bool).unwrap_or(false);
        let sharing = peer.get("sharing").and_then(Value::as_bool).unwrap_or(false);
        set(&self.shared, |s| {
            s.peer_cam_off = cam_off;
            s.peer_muted = muted;
            s.peer_sharing = sharing;
        });
    }

    fn members(&mut self, list: &Value) {
        let parsed = parse_member_list(list);
        set(&self.shared, |s| {
            if s.members != parsed {
                s.members = parsed;
                s.members_gen = s.members_gen.saturating_add(1);
            }
        });
    }

    fn notice(&mut self, text: &str) {
        if !text.is_empty() {
            let owned = text.to_owned();
            set(&self.shared, |s| s.notice = owned);
        }
    }

    fn request_key_frame(&mut self) {
        set(&self.shared, |s| s.key_requested = true);
    }

    fn speaking(&mut self, uid: u32, on: bool) {
        set(&self.shared, |s| {
            let changed = if on {
                s.speaking.insert(uid)
            } else {
                s.speaking.remove(&uid)
            };
            if changed {
                s.speaking_gen = s.speaking_gen.saturating_add(1);
            }
        });
    }

    fn mic_volume(&mut self, percent: u8) {
        set(&self.shared, |s| s.mic_volume = Some(percent));
    }
}

fn parse_member_list(list: &Value) -> Vec<MemberInfo> {
    let Some(items) = list.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let src = item.get("src").or_else(|| item.get("id"))?;
            let uid = src
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .or_else(|| src.as_str().and_then(|s| s.parse::<u32>().ok()))?;
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let avatar = item
                .get("avatar")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let muted = item.get("muted").and_then(Value::as_bool).unwrap_or(false);
            let cam_off = item.get("camOff").and_then(Value::as_bool).unwrap_or(false);
            let state = item
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("incall")
                .to_owned();
            Some(MemberInfo {
                uid,
                name,
                avatar,
                muted,
                cam_off,
                state,
            })
        })
        .collect()
}

fn set(shared: &Arc<Mutex<Shared>>, f: impl FnOnce(&mut Shared)) {
    if let Ok(mut s) = shared.lock() {
        f(&mut s);
    }
}

/// Control plane + UDP media + audio, as `zcall-native --media zrtc --send-udp`.
fn worker(
    shared: &Arc<Mutex<Shared>>,
    action_rx: Receiver<UiAction>,
    frame_rx: Receiver<Value>,
    job: Job,
    devices: (Option<String>, Option<String>),
    mut camera: Option<String>,
    dir: Option<std::path::PathBuf>,
    mic_volume: u8,
) {
    use zcall_engine::audio_bridge::AudioBridge;
    let verbose = call_log_enabled();
    let stub = std::env::var("ZALO_GPUI_CALL_MEDIA").as_deref() == Ok("stub");

    // Media: real UDP (ZRTP + SRTP audio) unless ZALO_GPUI_CALL_MEDIA=stub.
    let (audio_tx, audio_rx) = mpsc::channel::<(u32, u32, Vec<u8>)>();
    let (video_tx, video_rx) = mpsc::channel::<zcall_core::video::VideoFrame>();
    let mut zrtc: Option<Arc<Mutex<ZrtcMediaBackend>>> = None;
    let media: Box<dyn Media> = if stub {
        drop(audio_tx);
        drop(video_tx);
        Box::new(NoopMedia)
    } else {
        let mut backend = ZrtcMediaBackend::new(true);
        backend.set_log(move |msg: &str| {
            let watch = msg.contains("ZaviPing video")
                || msg.contains("group video out")
                || msg.contains("group video decrypted")
                || msg.contains("group video SRTP")
                || msg.contains("aes-cm probe");
            if verbose || watch {
                eprintln!("zalo-gpui call [media] {msg}");
            }
        });
        backend.set_on_audio_seq(move |seq: u32, ssrc: u32, opus: &[u8]| {
            let _ = audio_tx.send((seq, ssrc, opus.to_vec()));
        });
        backend.set_on_video(move |frame| {
            let _ = video_tx.send(frame.clone());
        });
        let shared_backend = SharedZrtcBackend::new(backend);
        zrtc = Some(shared_backend.clone_inner());
        Box::new(shared_backend)
    };
    let mut engine = EngineCore::new(
        media,
        Box::new(CallUi {
            shared: Arc::clone(shared),
        }),
    );
    engine.group_enabled = true;
    engine.start();

    let (sig_tx, sig_rx) = mpsc::channel::<Value>();
    let (recv_tx, recv_rx) = mpsc::channel::<Value>();
    {
        let shared = Arc::clone(shared);
        std::thread::spawn(move || http_loop(dir, sig_rx, &shared, recv_tx, verbose));
    }
    if job.incoming {
        if let Some(ctrl) = job.control {
            engine.on_zalo_frame(&zalo_client::control_frame(&ctrl));
        }
    } else if job.group {
        let grid = job.peer_id.trim_start_matches("g_");
        engine.on_zalo_frame(&serde_json::json!({
            "type": "request",
            "command": "makeCall",
            "data": {
                "type": if job.video { 6 } else { 5 },
                "groupInfo": {
                    "id": grid,
                    "name": job.peer_name,
                    "groupName": job.peer_name,
                    "avatar": job.avatar,
                    "maxUsers": if job.max_users == 0 { 8 } else { job.max_users },
                },
                "partner": job.partners,
            }
        }));
    } else {
        engine.on_zalo_frame(&serde_json::json!({
            "type": "request",
            "command": "makeCall",
            "data": {
                "type": if job.video { 3 } else { 1 },
                "partner": [{ "id": job.peer_id, "name": job.peer_name }],
            }
        }));
    }
    let drain_out = |engine: &mut EngineCore, sig_tx: &Sender<Value>| {
        for frame in std::mem::take(&mut engine.frames) {
            if zalo_client::send_signal_parts(&frame).is_some() {
                let _ = sig_tx.send(frame);
            } else if verbose {
                let kind = frame
                    .get("command")
                    .or_else(|| frame.get("type"))
                    .map(Value::to_string)
                    .unwrap_or_default();
                eprintln!("zalo-gpui call: engine frame not sent ({kind})");
            }
        }
    };
    drain_out(&mut engine, &sig_tx);

    let mut bridge = AudioBridge::new();
    bridge.prepare_os_volume(mic_volume);
    if let Some((aec, ns)) = shared.lock().ok().and_then(|mut s| s.audio_dsp_req.take()) {
        bridge.set_audio_processing(aec, ns);
    }
    eprintln!("zalo-gpui call: mic volume {mic_volume}%");
    set(shared, |s| s.mic_volume = Some(mic_volume));
    if job.group {
        bridge.defer_speaker();
    }
    let audio = match bridge.start(None, devices.0.as_deref(), devices.1.as_deref(), false) {
        Ok(()) => true,
        Err(e) => {
            let msg = format!("Không mở được mic/loa: {e}");
            set(shared, |s| {
                s.notice = msg.clone();
                s.debug = msg;
            });
            false
        }
    };
    let started = Instant::now();
    let (mut last_ping, mut last_status, mut last_twcc, mut last_report) = (
        Instant::now(),
        Instant::now(),
        Instant::now(),
        Instant::now(),
    );
    let mut last_rx = 0_u64;
    let mut last_tx = 0_u64;
    let mut logged_rx = 0_u64;
    let mut logged_tx = 0_u64;
    let mut last_audio_log = Instant::now();
    let mut noted_audio = false;
    let mut connected_since: Option<Instant> = None;
    let mut had_call = engine.call.is_some();
    let mut last_state = String::new();
    let mut seen_uid = 0_u32;
    let mut ending: Option<Instant> = None;
    // Mic frames encoded while the call is still ringing are not sent.
    // Electron starts the microphone at media.start; dumping the backlog in
    // one tick is a burst the phone scores as a weak signal, and the first
    // seconds of the connected call are that old audio.
    let mut media_live = false;
    let mut delay_applied = false;
    // Speaker consumes one 20 ms frame per tick. The loop still polls every 5 ms.
    let mut next_play = Instant::now();
    // Dedicated remote H.264 decoder thread (`zcall-vdec`) so software video
    // decoding and YUV->BGRA conversion never stall UDP/audio ticks in 1:1 or
    // group video calls.
    let (need_key_tx, need_key_rx) = mpsc::channel::<u32>();
    let vdec_handle = {
        let shared = Arc::clone(shared);
        let group = job.group;
        std::thread::Builder::new()
            .name("zcall-vdec".into())
            .spawn(move || {
                let mut remote = crate::call_video::RemoteVideo::new();
                let mut shown: std::collections::BTreeMap<u32, crate::call_video::RemotePicture> =
                    std::collections::BTreeMap::new();
                let mut video_size: Option<(u32, u32, u32)> = None;
                let mut noted_remote = false;
                while let Ok(first) = video_rx.recv() {
                    let mut by_src: std::collections::BTreeMap<
                        u32,
                        Vec<zcall_core::video::VideoFrame>,
                    > = std::collections::BTreeMap::new();
                    by_src.entry(first.src).or_default().push(first);
                    while let Ok(frame) = video_rx.try_recv() {
                        by_src.entry(frame.src).or_default().push(frame);
                    }
                    for (src, mut frames) in by_src {
                        if frames.len() > 1 {
                            if let Some(from) = frames.iter().rposition(|frame| frame.key) {
                                frames.drain(..from);
                            }
                        }
                        if frames.is_empty() {
                            continue;
                        }
                        // Decode earlier queued deltas in decode-only mode so
                        // H.264 reference state stays intact without triggering
                        // a PLI freeze or paying YUV→BGRA conversion on dropped
                        // frames; only convert and publish the latest frame.
                        let last = frames.pop().expect("non-empty batch");
                        let mut need_key_sent = false;
                        for frame in &frames {
                            if let Some(need_src) = remote.push_decode_only(frame) {
                                if !need_key_sent && (src != 0 || !group) {
                                    let _ = need_key_tx.send(need_src);
                                    need_key_sent = true;
                                }
                            }
                        }
                        match remote.push(&last) {
                            Some(crate::call_video::Shown::Picture { src, picture }) => {
                                let size = (src, picture.w, picture.h);
                                if !noted_remote {
                                    eprintln!(
                                        "zalo-gpui call: remote video {}x{} from {src}",
                                        picture.w, picture.h
                                    );
                                    noted_remote = true;
                                }
                                if verbose && video_size != Some(size) {
                                    eprintln!(
                                        "zalo-gpui call: video {}x{} from {src}",
                                        picture.w, picture.h
                                    );
                                    video_size = Some(size);
                                }
                                shown.insert(src, picture);
                                let pictures: Vec<_> = shown
                                    .iter()
                                    .map(|(&id, pic)| (id, pic.clone()))
                                    .collect();
                                set(&shared, |s| {
                                    s.pictures = pictures;
                                    s.picture_gen = s.picture_gen.saturating_add(1);
                                });
                            }
                            Some(crate::call_video::Shown::NeedKey(need_src)) => {
                                if !need_key_sent {
                                    let _ = need_key_tx.send(need_src);
                                }
                            }
                            None => {}
                        }
                    }
                }
            })
            .ok()
    };
    let mut last_tile_gen = 0_u64;
    let mut last_cam_profile: Option<zcall_core::send_rate::Profile> = None;
    let mut cam = LocalCam {
        wanted: job.video && !job.incoming,
        failed: false,
        reported: false,
        cap: None,
    };
    let mut screen_cap: Option<crate::call_screen::ScreenCapture> = None;
    let mut screen_on = false;
    loop {
        let mut paced_pending = false;
        while let Ok(action) = action_rx.try_recv() {
            match action {
                UiAction::Mute { on } => {
                    engine.mute(on);
                    bridge.set_mute(on);
                    set(shared, |s| s.local_muted = on);
                }
                UiAction::Speaker { off } => {
                    engine.speaker(off);
                    bridge.set_speaker(off);
                }
                UiAction::Accept { cam_start_off } => {
                    cam.wanted = job.video && !cam_start_off;
                    set(shared, |s| s.camera_on = cam.wanted);
                    engine.answer(cam_start_off);
                }
                UiAction::Camera { on } => {
                    cam.wanted = on && job.video;
                    if on {
                        cam.failed = false;
                    }
                    set(shared, |s| s.camera_on = on && job.video);
                }
                UiAction::Reject { why } => {
                    engine.reject(&why);
                    ending.get_or_insert_with(Instant::now);
                }
                UiAction::Hangup => {
                    engine.hangup("local");
                    ending.get_or_insert_with(Instant::now);
                }
                UiAction::MicVolume { percent } => {
                    let pct = percent.clamp(0, 100) as u8;
                    if bridge.os_volume_percent() != pct {
                        eprintln!("zalo-gpui call: mic volume {pct}%");
                    }
                    bridge.set_os_mic_volume(pct);
                    set(shared, |s| s.mic_volume = Some(pct));
                }
                UiAction::Screen { on } => {
                    let connected = engine
                        .call
                        .as_ref()
                        .is_some_and(|call| call.state == "connected");
                    if on && !connected {
                        screen_on = false;
                        screen_cap.take();
                        set(shared, |s| {
                            s.sharing = false;
                            s.notice = "Chỉ chia sẻ khi cuộc gọi đã nối".into();
                        });
                    } else if on && crate::call_screen::grab_unavailable().is_some() {
                        // Do not signal the peer, and do not start a grab that
                        // can take the call thread down. The window stays open.
                        let text = crate::call_screen::grab_unavailable()
                            .unwrap_or_else(|| "Không chia sẻ được màn hình".into());
                        screen_on = false;
                        screen_cap.take();
                        set(shared, |s| {
                            s.sharing = false;
                            s.notice = text.clone();
                            s.debug = text;
                        });
                    } else {
                        let group = engine.call.as_ref().is_some_and(|call| call.group);
                        if on {
                            if !group {
                                // The phone shows our picture only while it thinks the camera is on.
                                engine._cam_mic(Some(true));
                            }
                            let target = shared
                                .lock()
                                .ok()
                                .map(|s| s.share_target.clone())
                                .unwrap_or_default();
                            let target_label = target.label();
                            match crate::call_screen::ScreenCapture::start(target) {
                                Ok(cap) => {
                                    screen_cap = Some(cap);
                                    screen_on = true;
                                    engine.screen_share(true);
                                    set(shared, |s| {
                                        s.share_label = target_label;
                                        s.notice = "Đang chia sẻ màn hình".into();
                                        if s.debug.to_lowercase().contains("camera") {
                                            s.debug.clear();
                                        }
                                    });
                                }
                                Err(text) => {
                                    screen_on = false;
                                    screen_cap.take();
                                    set(shared, |s| {
                                        s.notice = text.clone();
                                        s.debug = text;
                                    });
                                }
                            }
                        } else {
                            screen_on = false;
                            screen_cap.take();
                            engine.screen_share(false);
                            if !group && !cam.wanted {
                                engine._cam_mic(Some(false));
                            }
                            set(shared, |s| {
                                s.notice.clear();
                                if !group {
                                    clear_local_preview(s);
                                }
                            });
                        }
                        set(shared, |s| s.sharing = screen_on);
                    }
                }
                UiAction::Device { kind, id } => {
                    let name = if id.is_empty() {
                        None
                    } else {
                        Some(id.as_str())
                    };
                    if verbose {
                        eprintln!(
                            "zalo-gpui call: switch {kind} -> {}",
                            name.unwrap_or("default")
                        );
                    }
                    let result = match kind.as_str() {
                        "mic" => bridge.set_source(name),
                        "speaker" => bridge.set_sink(name),
                        "camera" => {
                            camera = resolve_camera_selection(&id);
                            cam.failed = false;
                            let keep_on = cam.wanted;
                            cam.cap.take();
                            set(shared, |s| s.camera_on = keep_on);
                            Ok(())
                        }
                        _ => Ok(()),
                    };
                    if let Err(e) = result {
                        let msg = format!("Không đổi được thiết bị: {e}");
                        set(shared, |s| {
                            s.notice = msg.clone();
                            s.debug = msg;
                        });
                    }
                }
                _ => {}
            }
        }
        while let Ok(frame) = recv_rx.try_recv() {
            if verbose {
                eprintln!("zalo-gpui call: <- http {}", sanitized_call_json(&frame));
            }
            engine.on_zalo_frame(&frame);
        }
        while let Ok(frame) = frame_rx.try_recv() {
            if verbose {
                eprintln!("zalo-gpui call: <- voip {}", sanitized_call_json(&frame));
            }
            engine.on_zalo_frame(&frame);
        }
        // Handle local UI-only audio controls (speaker volume, test chime, AEC/NS)
        // and publish live mic VU level for "Device Status".
        {
            let (spk_vol_req, test_spk, dsp_req) = shared
                .lock()
                .ok()
                .map(|mut s| {
                    s.live_mic_level = bridge.live_mic_level();
                    (
                        s.speaker_volume_req.take(),
                        std::mem::take(&mut s.test_speaker_req),
                        s.audio_dsp_req.take(),
                    )
                })
                .unwrap_or((None, false, None));
            if let Some(pct) = spk_vol_req {
                bridge.set_speaker_volume(pct);
            }
            if test_spk {
                bridge.play_test_tone();
            }
            if let Some((aec, ns)) = dsp_req {
                bridge.set_audio_processing(aec, ns);
            }
        }
        engine.tick();
        drain_out(&mut engine, &sig_tx);

        // UDP media: poll, timers, mic → peer, peer → jitter buffer.
        if let Some(backend) = zrtc.as_ref() {
            let mut rx = None;
            let mut cam_profile = None;
            let mut key_requested = false;
            let mut loss_perc = None;
            let tile_updates = shared
                .lock()
                .ok()
                .filter(|s| s.tile_gen != last_tile_gen)
                .map(|s| {
                    last_tile_gen = s.tile_gen;
                    s.tile_widths.clone()
                });
            if let Ok(mut b) = backend.lock() {
                poll_and_tick(
                    &mut b,
                    &mut last_ping,
                    &mut last_status,
                    &mut last_twcc,
                    &mut last_report,
                );
                paced_pending = b.drain_video_queue(0);
                if let Some(widths) = tile_updates {
                    for (uid, width) in widths {
                        b.set_member_render_width(uid, width);
                    }
                }
                key_requested = b.take_key_frame_request()
                    || shared
                        .lock()
                        .ok()
                        .map(|mut s| std::mem::take(&mut s.key_requested))
                        .unwrap_or(false);
                cam_profile = b.current_video_profile();
                loss_perc = b.observed_loss_perc();
                if audio {
                    let up = b.is_started();
                    if !up {
                        while bridge.next_audio_out().is_some() {}
                        media_live = false;
                    } else if !media_live {
                        while bridge.next_audio_out().is_some() {}
                        media_live = true;
                    } else {
                        while let Some(out) = bridge.next_audio_out() {
                            b.send_audio(&out.opus, out.level);
                        }
                    }
                }
                rx = Some((b.rx_count(), b.tx_count()));
            }
            if audio {
                if let Some(loss) = loss_perc {
                    bridge.set_packet_loss_perc(loss);
                }
            }
            if let Some(cap) = cam.cap.as_ref() {
                if key_requested {
                    cap.request_key_frame();
                }
                if let Some(profile) = cam_profile {
                    if last_cam_profile != Some(profile) {
                        last_cam_profile = Some(profile);
                        cap.set_profile(profile);
                    }
                }
            }
            if key_requested {
                if let Some(cap) = screen_cap.as_ref() {
                    cap.request_key_frame();
                }
            }
            if let Some((rx, tx)) = rx {
                if last_rx < 1 && rx >= 1 {
                    engine.early_audio("first");
                }
                if last_rx < 50 && rx >= 50 {
                    engine.early_audio("stream");
                }
                last_rx = rx;
                last_tx = tx;
                if verbose
                    && last_audio_log.elapsed() >= Duration::from_secs(2)
                    && (rx != logged_rx || tx != logged_tx)
                {
                    eprintln!("zalo-gpui call: audio rx={rx} tx={tx}");
                    logged_rx = rx;
                    logged_tx = tx;
                    last_audio_log = Instant::now();
                }
            }
            if !delay_applied {
                if let Some(cfg) = engine
                    .call
                    .as_ref()
                    .map(|c| c.config.clone())
                    .filter(|c| c.as_object().is_some_and(|o| !o.is_empty()))
                {
                    let (min_ms, max_ms) = zcall_core::zrtc_helpers::playout_delay_ms(&cfg);
                    bridge.apply_config(job.group, &cfg);
                    delay_applied = true;
                    if verbose {
                        eprintln!("zalo-gpui call: playout delay {min_ms}..{max_ms} ms");
                    }
                }
            }
            let group = engine.call.as_ref().is_some_and(|c| c.group);
            while let Ok((seq, ssrc, opus)) = audio_rx.try_recv() {
                if audio {
                    let source = (ssrc != 0).then_some(ssrc);
                    bridge.push_audio_in(
                        seq,
                        &opus,
                        source,
                        zcall_engine::audio_bridge::monotonic_secs(),
                    );
                }
            }
            let mut need_key: Vec<u32> = Vec::new();
            while let Ok(src) = need_key_rx.try_recv() {
                if !need_key.contains(&src) {
                    need_key.push(src);
                }
            }
            if !need_key.is_empty() {
                if let Ok(mut b) = backend.lock() {
                    for src in need_key {
                        let who = group.then_some(src).filter(|id| *id != 0);
                        b.request_peer_key_frame("decoder", who);
                    }
                }
            }
        } else if audio {
            while bridge.next_audio_out().is_some() {}
        }
        let group_call = engine.call.as_ref().is_some_and(|call| call.group);
        drive_camera(
            &mut cam,
            &mut engine,
            &zrtc,
            shared,
            job.video,
            camera.as_deref(),
            &mut last_cam_profile,
            screen_on && !group_call,
        );
        drive_screen(
            &mut screen_cap,
            &mut screen_on,
            &mut engine,
            &zrtc,
            shared,
            group_call,
        );
        if audio && next_play.elapsed() >= Duration::from_millis(20) {
            next_play = Instant::now();
            if bridge.tick_playout().is_err() {
                engine.audio_fault("speaker");
            }
        }

        match engine.call.as_ref() {
            Some(c) => {
                had_call = true;
                if c.local_uid != 0 && c.local_uid != seen_uid {
                    seen_uid = c.local_uid;
                    let uid = c.local_uid;
                    set(shared, |s| s.local_uid = uid);
                }
                if c.state != last_state {
                    last_state = c.state.clone();
                    if verbose {
                        eprintln!("zalo-gpui call: state {} ({})", c.state, c.status_text);
                    }
                    if c.state == "connected" {
                        connected_since.get_or_insert_with(Instant::now);
                    }
                    let (state, text) = (c.state.clone(), c.status_text.clone());
                    set(shared, |s| {
                        if state == "connected" && s.connected_at.is_none() {
                            s.connected_at = Some(Instant::now());
                        }
                        s.state = state;
                        if !text.is_empty() {
                            s.notice = text;
                        }
                    });
                }
            }
            None if had_call => break,
            None => {}
        }
        // After a local hang-up: give the HTTP thread a moment to send it.
        if ending.is_some_and(|t| t.elapsed() > Duration::from_millis(1500)) {
            break;
        }
        if !had_call && started.elapsed() > Duration::from_secs(20) {
            set(shared, |s| s.notice = "Không kết nối được cuộc gọi".into());
            break;
        }
        if verbose && !noted_audio {
            if let Some(at) = connected_since {
                if at.elapsed() >= Duration::from_secs(3) {
                    eprintln!("zalo-gpui call: audio rx={last_rx} tx={last_tx}");
                    logged_rx = last_rx;
                    logged_tx = last_tx;
                    last_audio_log = Instant::now();
                    noted_audio = true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(if paced_pending { 2 } else { 5 }));
    }
    if verbose {
        eprintln!("zalo-gpui call: ended, {last_rx} received, {last_tx} sent");
    }
    cam.cap.take();
    screen_cap.take();
    engine.media.stop();
    drop(engine);
    drop(zrtc);
    if let Some(handle) = vdec_handle {
        let _ = handle.join();
    }
    drop(sig_tx);
    bridge.stop();
}

/// Local camera for this call. `wanted` follows the pill and "Answer without camera".
struct LocalCam {
    wanted: bool,
    failed: bool,
    /// 12006 / the group camera flag already matches `wanted`.
    reported: bool,
    cap: Option<crate::call_camera::CameraCapture>,
}

fn preview_profile() -> zcall_core::send_rate::Profile {
    zcall_core::send_rate::Profile {
        width: 640,
        height: 360,
        fps: 15,
        bitrate: 500_000,
        key_ms: 10_000,
        capture_big: true,
        rung: 3,
    }
}

/// Starts local preview early on outgoing video calls and transmits frames once connected.
fn drive_camera(
    cam: &mut LocalCam,
    engine: &mut EngineCore,
    zrtc: &Option<Arc<Mutex<ZrtcMediaBackend>>>,
    shared: &Arc<Mutex<Shared>>,
    video: bool,
    preferred_camera: Option<&str>,
    last_cam_profile: &mut Option<zcall_core::send_rate::Profile>,
    hold_for_screen: bool,
) {
    if hold_for_screen {
        // 1-1 screen share replaces the camera picture. Leave the "camera on"
        // signal so the phone keeps showing the video tile.
        if cam.cap.take().is_some() {
            set(shared, clear_local_preview);
        }
        return;
    }
    let connected = engine
        .call
        .as_ref()
        .is_some_and(|call| call.state == "connected");
    let initial_profile = zrtc
        .as_ref()
        .and_then(|backend| backend.lock().ok())
        .filter(|backend| backend.is_started())
        .and_then(|backend| backend.current_video_profile());
    if cam.wanted && video && cam.cap.is_none() && !cam.failed {
        let profile = initial_profile.unwrap_or_else(preview_profile);
        if initial_profile.is_some() {
            *last_cam_profile = Some(profile);
        }
        match crate::call_camera::CameraCapture::start(profile, preferred_camera.map(str::to_owned))
        {
            Ok(cap) => {
                cam.cap = Some(cap);
                set(shared, |state| {
                    state.camera_on = true;
                    if state.debug.contains("camera") {
                        state.debug.clear();
                    }
                });
            }
            Err(text) => {
                cam.failed = true;
                cam.wanted = false;
                set(shared, |state| {
                    state.camera_on = false;
                    state.debug = text.clone();
                    state.notice = text;
                });
            }
        }
    }
    if cam.wanted && connected && initial_profile.is_some() && cam.cap.is_some() && !cam.reported {
        if let Some(backend) = zrtc {
            if let Ok(mut backend) = backend.lock() {
                backend.set_group_camera(true);
            }
        }
        engine._cam_mic(Some(true));
        cam.reported = true;
    }
    if !cam.wanted && (cam.cap.is_some() || cam.reported) {
        cam.cap.take();
        if let Some(backend) = zrtc {
            if let Ok(mut backend) = backend.lock() {
                backend.set_group_camera(false);
            }
        }
        if cam.reported {
            engine._cam_mic(Some(false));
            cam.reported = false;
        }
        set(shared, |state| {
            state.camera_on = false;
            clear_local_preview(state);
        });
    }
    let mut failure = None;
    if let Some(cap) = cam.cap.as_ref() {
        if let Some(preview) = cap.take_preview() {
            set(shared, |state| {
                state.local_preview = Some(crate::call_video::RemotePicture {
                    w: preview.w,
                    h: preview.h,
                    bgra: preview.bgra,
                });
                state.local_preview_gen = state.local_preview_gen.wrapping_add(1);
            });
        }
        while let Some(event) = cap.poll() {
            match event {
                crate::call_camera::CamEvent::Frame { key, data } => {
                    if let Some(backend) = zrtc {
                        if let Ok(mut backend) = backend.lock() {
                            if backend.is_started() {
                                backend.send_video(key, &data, false);
                            }
                        }
                    }
                }
                crate::call_camera::CamEvent::Failed(text) => {
                    failure = Some(text);
                    break;
                }
            }
        }
    }
    if let Some(text) = failure {
        cam.cap.take();
        cam.failed = true;
        cam.wanted = false;
        if let Some(backend) = zrtc {
            if let Ok(mut backend) = backend.lock() {
                backend.set_group_camera(false);
            }
        }
        if cam.reported {
            engine._cam_mic(Some(false));
            cam.reported = false;
        }
        set(shared, |state| {
            state.camera_on = false;
            state.debug = text.clone();
            state.notice = text;
            clear_local_preview(state);
        });
    }
}

fn clear_local_preview(state: &mut Shared) {
    if state.local_preview.is_some() {
        state.local_preview = None;
        state.local_preview_gen = state.local_preview_gen.wrapping_add(1);
    }
}

/// Sends encoded screen frames. 1-1 uses the camera SSRC. A group uses the
/// share peer once `shareScreenId` has arrived.
fn drive_screen(
    screen_cap: &mut Option<crate::call_screen::ScreenCapture>,
    screen_on: &mut bool,
    engine: &mut EngineCore,
    zrtc: &Option<Arc<Mutex<ZrtcMediaBackend>>>,
    shared: &Arc<Mutex<Shared>>,
    group: bool,
) {
    let Some(cap) = screen_cap.as_ref() else {
        return;
    };
    if let Some(preview) = cap.take_preview() {
        if group {
            if let Some(backend) = zrtc {
                if let Ok(mut b) = backend.lock() {
                    if let (Ok(w), Ok(h)) = (u16::try_from(preview.w), u16::try_from(preview.h)) {
                        b.set_share_dimensions(w, h);
                    }
                }
            }
        }
        set(shared, |state| {
            state.local_preview = Some(crate::call_video::RemotePicture {
                w: preview.w,
                h: preview.h,
                bgra: preview.bgra,
            });
            state.local_preview_gen = state.local_preview_gen.wrapping_add(1);
        });
    }
    if group {
        if let Some(backend) = zrtc {
            if let Ok(mut b) = backend.lock() {
                if b.take_share_key_frame_request() {
                    cap.request_key_frame();
                }
            }
        }
    }
    let mut failure = None;
    while let Some(event) = cap.poll() {
        match event {
            crate::call_camera::CamEvent::Frame { key, data } => {
                let share_ready = engine
                    .call
                    .as_ref()
                    .is_some_and(|call| call.share_screen_id > 0);
                // Group frames wait for the share peer. 1-1 replaces the camera.
                if group && !share_ready {
                    continue;
                }
                if let Some(backend) = zrtc {
                    if let Ok(mut backend) = backend.lock() {
                        if backend.is_started() {
                            backend.send_video(key, &data, group);
                        }
                    }
                }
            }
            crate::call_camera::CamEvent::Failed(text) => {
                failure = Some(text);
                break;
            }
        }
    }
    if let Some(text) = failure {
        screen_cap.take();
        *screen_on = false;
        engine.screen_share(false);
        set(shared, |state| {
            state.sharing = false;
            state.notice = text.clone();
            state.debug = text;
        });
    }
}

/// Returns true when `ZALO_GPUI_CALL_LOG` is set to inspect call signaling.
pub(crate) fn call_log_enabled() -> bool {
    std::env::var_os("ZALO_GPUI_CALL_LOG").is_some()
}

fn is_safe_scalar_key(key: Option<&str>) -> bool {
    let Some(k) = key else {
        return false;
    };
    let lower = k.to_ascii_lowercase();
    lower.starts_with("enable")
        || lower.starts_with("support")
        || lower.starts_with("allow")
        || matches!(
            lower.as_str(),
            "act"
                | "act_type"
                | "type"
                | "command"
                | "cmd"
                | "sub_cmd"
                | "error_code"
                | "error_message"
                | "error_msg"
                | "errmsg"
                | "reason"
                | "status"
                | "codec"
                | "platform"
                | "calltype"
                | "call_type"
                | "subtype"
                | "sub_type"
                | "exttype"
                | "ext_type"
                | "action"
                | "mode"
                | "state"
        )
}

fn is_masked_number_key(key: Option<&str>) -> bool {
    let Some(k) = key else {
        return false;
    };
    let lower = k.to_ascii_lowercase();
    matches!(lower.as_str(), "port" | "ts" | "time" | "uid" | "uidn") || lower.ends_with("id")
}

/// Sanitized JSON shape for call logs: keeps object keys, booleans, flags, and
/// embedded JSON structure (`params`, `extendData`), while masking IDs/IPs/secrets.
pub(crate) fn sanitized_call_json(v: &Value) -> String {
    fn walk(v: &Value, key: Option<&str>, depth: u8, out: &mut String) {
        match v {
            Value::Object(m) if depth < 7 => {
                out.push('{');
                for (i, (k, x)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    out.push_str(k);
                    out.push(':');
                    walk(x, Some(k.as_str()), depth + 1, out);
                }
                out.push('}');
            }
            Value::Object(_) => out.push_str("{…}"),
            Value::Array(a) if depth < 7 && !a.is_empty() => {
                out.push('[');
                for (i, x) in a.iter().take(5).enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    walk(x, key, depth + 1, out);
                }
                if a.len() > 5 {
                    out.push_str(&format!(" +{}", a.len() - 5));
                }
                out.push(']');
            }
            Value::Array(a) => out.push_str(&format!("[{}]", a.len())),
            Value::String(s) => {
                let trimmed = s.trim();
                let allow_nested_parse = key.is_none_or(|k| {
                    let lower = k.to_ascii_lowercase();
                    !lower.contains("token")
                        && !lower.contains("cookie")
                        && !lower.contains("secret")
                });
                if depth < 7
                    && allow_nested_parse
                    && (trimmed.starts_with('{') || trimmed.starts_with('['))
                {
                    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                        walk(&parsed, key, depth + 1, out);
                        return;
                    }
                }
                let digits_id = s.len() >= 6 && s.chars().all(|c| c.is_ascii_digit());
                if is_safe_scalar_key(key) && !digits_id {
                    let shown: String = s.chars().take(64).collect();
                    out.push_str(&shown);
                    if s.chars().count() > 64 {
                        out.push('…');
                    }
                } else {
                    out.push_str(&format!("str{}", s.len()));
                }
            }
            Value::Number(n)
                if !is_masked_number_key(key) && n.as_i64().is_some_and(|n| n.abs() < 100_000) =>
            {
                out.push_str(&n.to_string());
            }
            Value::Number(_) => out.push('#'),
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    walk(v, None, 0, &mut out);
    out
}

/// `sendSignal` frames → GET `/api/voicecall/*`; replies back as `recvSignal`.
fn http_loop(
    dir: Option<std::path::PathBuf>,
    rx: Receiver<Value>,
    shared: &Arc<Mutex<Shared>>,
    recv_tx: Sender<Value>,
    verbose: bool,
) {
    let Some(dir) = dir else {
        return;
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let api = rt.block_on(async {
        let device = zalo_client::Device::load_or_create(&dir).map_err(|e| e.to_string())?;
        let session = zalo_client::StoredSession::load(&dir).map_err(|e| e.to_string())?;
        let mut api =
            zalo_client::ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
        api.get_login_info().await.map_err(|e| e.to_string())?;
        Ok::<_, String>(api)
    });
    let api = match api {
        Ok(api) => api,
        Err(e) => {
            set(shared, |s| {
                s.notice = "Không gửi được tín hiệu gọi".into();
                s.debug = e;
                s.ended = true;
            });
            return;
        }
    };
    while let Ok(frame) = rx.recv() {
        let Some((command, data)) = zalo_client::send_signal_parts(&frame) else {
            continue;
        };
        if verbose {
            eprintln!(
                "zalo-gpui call: -> http cmd={command} {}",
                sanitized_call_json(&data)
            );
        }
        // One retry: a pooled connection the server already closed fails fast.
        let mut result = rt.block_on(api.send_voicecall_signal(command, &data));
        if result.is_err() {
            std::thread::sleep(Duration::from_millis(300));
            result = rt.block_on(api.send_voicecall_signal(command, &data));
        }
        match result {
            Ok(Some(body)) => {
                if verbose {
                    eprintln!(
                        "zalo-gpui call: <- http_raw cmd={command} {}",
                        sanitized_call_json(&body)
                    );
                }
                let _ = recv_tx.send(zalo_client::recv_signal_from_http(command, &body));
            }
            Ok(None) => {
                if verbose {
                    eprintln!("zalo-gpui call: -> http cmd={command} unmapped");
                }
            }
            Err(e) => {
                eprintln!("zalo-gpui call: signal {command} failed: {e}");
                set(shared, |s| s.debug = format!("tín hiệu {command}: {e}"));
            }
        }
    }
}

/// Available `(width, height)` for each video tile inside the call viewport.
#[cfg(test)]
pub(crate) fn tile_size_in_viewport(vp_w: f32, vp_h: f32, cols: usize, rows: usize) -> (f32, f32) {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let avail_w = (vp_w - 2.0 * (cols - 1) as f32).max(1.0);
    let avail_h = (vp_h - 50.0 - 2.0 * (rows - 1) as f32).max(1.0);
    (
        (avail_w / cols as f32).floor().max(1.0),
        (avail_h / rows as f32).floor().max(1.0),
    )
}

/// Computes `object-fit: contain` bounds inside `(box_w, box_h)` so GPUI layout sees definite image dimensions.
pub(crate) fn fit_image_contain(box_w: f32, box_h: f32, img_w: f32, img_h: f32) -> (f32, f32) {
    if box_w <= 0.0 || box_h <= 0.0 || img_w <= 0.0 || img_h <= 0.0 {
        return (box_w.max(1.0), box_h.max(1.0));
    }
    let scale = (box_w / img_w).min(box_h / img_h);
    (
        (img_w * scale).floor().max(1.0),
        (img_h * scale).floor().max(1.0),
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TileRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Clone)]
struct StageTile {
    target: PinTarget,
    name: String,
    avatar: Option<Arc<RenderImage>>,
    blur: Option<Arc<RenderImage>>,
    status: String,
    image: Option<Arc<RenderImage>>,
    muted: bool,
    speaking: bool,
    contain: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CallBarState {
    show_screen: bool,
    show_full: bool,
    show_layout: bool,
    cam_disabled: bool,
}

fn call_bar_state(
    video: bool,
    group: bool,
    up: bool,
    has_pictures: bool,
    sharing: bool,
    ended: bool,
) -> CallBarState {
    CallBarState {
        show_screen: (video || group) && up && !ended,
        show_full: (video || group) && !ended,
        show_layout: !ended && up && (group || has_pictures || sharing),
        cam_disabled: !video || (sharing && !group),
    }
}

fn group_grid_layout(stage_w: f32, stage_h: f32, count: usize) -> Vec<TileRect> {
    if count == 0 {
        return Vec::new();
    }
    const GAP: f32 = 4.0;
    const ASPECT: f32 = 820.0 / 482.0;
    let sw = stage_w.max(1.0);
    let sh = stage_h.max(1.0);
    let mut best_cols = 1usize;
    let mut best_area = 0.0_f32;
    let mut best_w = sw;
    let mut best_h = sh;
    for cols in 1..=count {
        let rows = count.div_ceil(cols);
        let max_cw = (sw - GAP * (cols - 1) as f32) / cols as f32;
        let max_ch = (sh - GAP * (rows - 1) as f32) / rows as f32;
        if max_cw <= 0.0 || max_ch <= 0.0 {
            continue;
        }
        let (cw, ch) = if max_cw / max_ch > ASPECT {
            (max_ch * ASPECT, max_ch)
        } else {
            (max_cw, max_cw / ASPECT)
        };
        let area = cw * ch;
        if area > best_area + 0.5 {
            best_area = area;
            best_cols = cols;
            best_w = cw.floor().max(1.0);
            best_h = ch.floor().max(1.0);
        }
    }
    let rows = count.div_ceil(best_cols);
    let grid_h = rows as f32 * best_h + (rows - 1) as f32 * GAP;
    let top0 = ((sh - grid_h) * 0.5).floor().max(0.0);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let r = i / best_cols;
        let c = i % best_cols;
        let row_items = if r + 1 == rows {
            count - r * best_cols
        } else {
            best_cols
        };
        let row_w = row_items as f32 * best_w + (row_items - 1) as f32 * GAP;
        let left0 = ((sw - row_w) * 0.5).floor().max(0.0);
        out.push(TileRect {
            x: left0 + c as f32 * (best_w + GAP),
            y: top0 + r as f32 * (best_h + GAP),
            w: best_w,
            h: best_h,
        });
    }
    out
}

fn group_conference_layout(
    stage_w: f32,
    stage_h: f32,
    count: usize,
    main_idx: usize,
) -> Vec<TileRect> {
    if count == 0 {
        return Vec::new();
    }
    let sw = stage_w.max(1.0);
    let sh = stage_h.max(1.0);
    if count == 1 {
        return vec![TileRect {
            x: 0.0,
            y: 0.0,
            w: sw,
            h: sh,
        }];
    }
    let main_idx = main_idx.min(count - 1);
    let col_gap = 8.0_f32;
    let side_w = (sw * 0.219)
        .clamp(150.0, 320.0)
        .min((sw * 0.42).max(120.0))
        .floor();
    let main_w = (sw - side_w - col_gap).max(1.0).floor();
    let thumbs = count - 1;
    let thumb_gap = if thumbs >= 5 { 2.0 } else { 4.0 };
    let nat_h = (side_w * 285.0 / 437.0).floor();
    let max_h = ((sh - thumb_gap * thumbs.saturating_sub(1) as f32) / thumbs as f32)
        .floor()
        .max(40.0);
    let thumb_h = nat_h.min(max_h);
    let total_h = thumbs as f32 * thumb_h + thumbs.saturating_sub(1) as f32 * thumb_gap;
    let top0 = ((sh - total_h) * 0.5).floor().max(0.0);
    let side_x = main_w + col_gap;
    let mut out = vec![TileRect {
        x: 0.0,
        y: 0.0,
        w: main_w,
        h: sh,
    }; count];
    let mut slot = 0usize;
    for (idx, rect) in out.iter_mut().enumerate() {
        if idx == main_idx {
            *rect = TileRect {
                x: 0.0,
                y: 0.0,
                w: main_w,
                h: sh,
            };
        } else {
            *rect = TileRect {
                x: side_x,
                y: top0 + slot as f32 * (thumb_h + thumb_gap),
                w: side_w,
                h: thumb_h,
            };
            slot += 1;
        }
    }
    out
}

fn split_1v1_layout(stage_w: f32, stage_h: f32) -> (TileRect, TileRect) {
    const GAP: f32 = 4.0;
    let sw = stage_w.max(1.0);
    let sh = stage_h.max(1.0);
    let w = ((sw - GAP) * 0.5).floor().max(1.0);
    (
        TileRect {
            x: 0.0,
            y: 0.0,
            w,
            h: sh,
        },
        TileRect {
            x: w + GAP,
            y: 0.0,
            w,
            h: sh,
        },
    )
}

/// Maps RFC 6464 dBov level (0 loudest .. 127 silence) to 0..=16 active VU dots.
fn mic_level_to_dots(level: u8, muted: bool) -> u8 {
    if muted || level >= 68 {
        return 0;
    }
    if level <= 14 {
        return 16;
    }
    let span = 68 - 14;
    let inv = u16::from(68 - level);
    ((inv * 16 + span / 2) / span).clamp(1, 16) as u8
}

fn render_video_contain(image: Arc<RenderImage>, box_w: f32, box_h: f32) -> gpui::AnyElement {
    let size = image.size(0);
    let img_w = i32::from(size.width) as f32;
    let img_h = i32::from(size.height) as f32;
    let (fit_w, fit_h) = fit_image_contain(box_w, box_h, img_w, img_h);
    img(image)
        .w(px(fit_w))
        .h(px(fit_h))
        .object_fit(gpui::ObjectFit::Contain)
        .into_any_element()
}

fn render_video_fit(image: Arc<RenderImage>, box_w: f32, box_h: f32) -> gpui::AnyElement {
    let size = image.size(0);
    let img_w = i32::from(size.width) as f32;
    let img_h = i32::from(size.height) as f32;
    if (img_w >= img_h) == (box_w >= box_h) {
        img(image)
            .w(px(box_w))
            .h(px(box_h))
            .object_fit(gpui::ObjectFit::Cover)
            .into_any_element()
    } else {
        let (fit_w, fit_h) = fit_image_contain(box_w, box_h, img_w, img_h);
        img(image)
            .w(px(fit_w))
            .h(px(fit_h))
            .object_fit(gpui::ObjectFit::Contain)
            .into_any_element()
    }
}

fn avatar_circle(
    name: &str,
    avatar: Option<Arc<RenderImage>>,
    d: f32,
    ring: f32,
    ring_color: gpui::Hsla,
) -> gpui::AnyElement {
    let initial = name.chars().next().unwrap_or('?').to_string();
    div()
        .size(px(d))
        .rounded(px(d / 2.))
        .overflow_hidden()
        .bg(rgb(0x2a_2a_2a))
        .border(px(ring))
        .border_color(ring_color)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(d * 0.38))
        .font_family(FAMILY_SEMIBOLD)
        .text_color(rgb(0xcf_d3_d8))
        .map(|el| match avatar {
            Some(a) => el.child(
                img(a)
                    .size(px((d - ring * 2.).max(1.)))
                    .rounded(px(d / 2.))
                    .object_fit(gpui::ObjectFit::Cover),
            ),
            None => el.child(initial),
        })
        .into_any_element()
}

/// Extracts the V4L2 device node path from a saved/selected camera option label.
pub(crate) fn resolve_camera_selection(choice: &str) -> Option<String> {
    crate::call_camera::parse_camera_path(choice)
}

/// Evaluates `incomingCall`, `needMic`, and `needSpeaker` call settings.
pub(crate) fn should_ring_incoming(
    incoming_call: bool,
    need_mic: bool,
    need_speaker: bool,
    has_mic: bool,
    has_speaker: bool,
) -> bool {
    if !incoming_call {
        return false;
    }
    if need_mic && !has_mic {
        return false;
    }
    if need_speaker && !has_speaker {
        return false;
    }
    true
}

/// Extensions the Linux `advanced-settings` plugin accepts.
pub(crate) const CALL_AUDIO_EXTS: &[&str] =
    &["mp3", "wav", "ogg", "m4a", "aac", "flac", "opus", "webm"];

/// Absolute readable audio path, or empty for Zalo's default.
///
/// # Errors
///
/// Returns the same messages as the Electron plugin when the path is relative,
/// the extension is not audio, or the file cannot be read.
pub(crate) fn validate_call_sound(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let path = std::path::Path::new(value);
    if !path.is_absolute() {
        return Err("Hãy nhập đường dẫn tuyệt đối đến tệp âm thanh.".into());
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !CALL_AUDIO_EXTS.contains(&ext.as_str()) {
        return Err("Định dạng âm thanh không được hỗ trợ.".into());
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(clean_abs(path)),
        _ => {
            Err("Không thể đọc tệp âm thanh. Hãy kiểm tra lại đường dẫn và quyền truy cập.".into())
        }
    }
}

fn clean_abs(path: &std::path::Path) -> String {
    let mut out = std::path::PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.display().to_string()
}

/// Locate a bundled stock ringtone/effect MP3 across installed runtime dirs or dev tree.
pub(crate) fn stock_sound_path(file: &str) -> Option<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data) = zalo_shell::data_home() {
        dirs.push(data.join("zalo-rust").join("zcall-sound"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("zcall-sound"));
            #[cfg(target_os = "macos")]
            dirs.push(parent.join("../Resources/zalo-rust/zcall-sound"));
        }
    }
    dirs.push(std::path::PathBuf::from("/usr/share/zalo-rust/zcall-sound"));
    dirs.push(std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../assets/sounds"
    )));
    dirs.into_iter().map(|d| d.join(file)).find(|p| p.is_file())
}

/// Active stock sound files needed at runtime for notifications and calls.
pub(crate) fn runtime_sound_paths() -> Vec<std::path::PathBuf> {
    ["zalo_ringtone.mp3", "zalo_ringback.mp3", "endcall.mp3"]
        .into_iter()
        .filter_map(stock_sound_path)
        .collect()
}

/// Custom file when it is valid, otherwise the stock clip for `kind`.
pub(crate) fn resolve_call_sound(kind: &str, custom: &str) -> Option<std::path::PathBuf> {
    if let Ok(path) = validate_call_sound(custom) {
        if !path.is_empty() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    let file = match kind {
        "ringtone" => "zalo_ringtone.mp3",
        "ringback" => "zalo_ringback.mp3",
        "end" => "endcall.mp3",
        _ => return None,
    };
    stock_sound_path(file)
}

/// Plays `path` for two seconds in-process. Used by Settings › Advanced › Preview.
pub(crate) fn preview_call_sound(path: &std::path::Path) {
    if cfg!(test) || !path.is_file() {
        return;
    }
    crate::audio_play::play_detached(
        path.to_path_buf(),
        None,
        Some(Duration::from_secs(2)),
    );
}

/// Plays a brief test chime on the chosen Pulse/PipeWire sink (or default device) for Settings › Call.
pub(crate) fn play_speaker_test(sink_name: Option<&str>) {
    if cfg!(test) {
        return;
    }
    let Some(path) = stock_sound_path("zalo_ringtone.mp3") else {
        return;
    };
    let sink = sink_name
        .filter(|s| !s.is_empty() && *s != "Mặc định")
        .map(str::to_owned);
    crate::audio_play::play_detached(path, sink, Some(Duration::from_millis(1400)));
}

#[cfg(test)]
mod tests {
    use super::{
        call_bar_state, due_prefix, fit_image_contain, group_conference_layout, group_grid_layout,
        is_selectable_pulse_device, mic_percent_from_json, parse_member_list, pulse_source_name,
        resolve_call_sound, resolve_camera_selection, sanitized_call_json, should_ring_incoming,
        tile_size_in_viewport, validate_call_sound, GPU_PICTURE_HOLD,
    };

    #[test]
    fn replaced_call_frames_stay_alive_for_the_gpu_hold() {
        let start = std::time::Instant::now();
        let held = vec![
            (start, 1_u8),
            (start + GPU_PICTURE_HOLD / 2, 2),
            (start + GPU_PICTURE_HOLD, 3),
        ];
        assert_eq!(due_prefix(&held, start), 1);
        assert_eq!(due_prefix(&held, start + GPU_PICTURE_HOLD / 2), 2);
        assert_eq!(due_prefix(&held, start + GPU_PICTURE_HOLD), 3);
    }

    #[test]
    fn call_sound_paths_match_the_linux_plugin() {
        assert!(validate_call_sound("").unwrap().is_empty());
        assert!(validate_call_sound("relative.mp3").is_err());
        assert!(validate_call_sound("/tmp/nope.txt").is_err());
        assert!(validate_call_sound("/tmp/missing-zalo-sound.mp3").is_err());
        let path = std::env::temp_dir().join("zalo-sound-ok.mp3");
        std::fs::write(&path, b"x").unwrap();
        let got = validate_call_sound(&path.display().to_string()).unwrap();
        assert!(got.ends_with("zalo-sound-ok.mp3"));
        let resolved = resolve_call_sound("ringtone", &got).unwrap();
        assert_eq!(resolved, std::path::PathBuf::from(&got));
        let fallback = resolve_call_sound("ringback", "not-absolute.wav").unwrap();
        assert!(fallback.ends_with("zalo_ringback.mp3"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn selectable_pulse_device_skips_monitors_and_echo_cancel_nodes() {
        assert!(!is_selectable_pulse_device("alsa_output.pci.monitor"));
        assert!(!is_selectable_pulse_device("zcall-ec-12345-1_source"));
        assert!(!is_selectable_pulse_device("zcall-ec-12345-1_sink"));
        assert!(is_selectable_pulse_device(
            "alsa_input.usb-Mic-00.mono-fallback"
        ));
        assert!(is_selectable_pulse_device(
            "alsa_output.pci-0000_00_1f.3.analog-stereo"
        ));
    }

    #[test]
    fn unset_mic_volume_is_full_scale() {
        assert_eq!(mic_percent_from_json(None), 100);
        assert_eq!(mic_percent_from_json(Some(&serde_json::json!(40))), 40);
        assert_eq!(mic_percent_from_json(Some(&serde_json::json!(140))), 100);
        assert_eq!(mic_percent_from_json(Some(&serde_json::json!(-3))), 100);
        assert_eq!(pulse_source_name(""), "@DEFAULT_SOURCE@");
        assert_eq!(pulse_source_name("Mặc định"), "@DEFAULT_SOURCE@");
        assert_eq!(pulse_source_name("alsa_input.usb"), "alsa_input.usb");
    }

    #[test]
    fn tile_size_in_viewport_accounts_for_bottom_bar_and_gaps() {
        assert_eq!(tile_size_in_viewport(760.0, 560.0, 1, 1), (760.0, 510.0));
        assert_eq!(tile_size_in_viewport(760.0, 560.0, 2, 2), (379.0, 254.0));
    }

    #[test]
    fn fit_image_contain_preserves_portrait_and_landscape_aspect_ratio() {
        // Portrait 720x1280 phone stream inside 760x510 tile must fit height, not zoom to 760px wide.
        let (pw, ph) = fit_image_contain(760.0, 510.0, 720.0, 1280.0);
        assert_eq!((pw, ph), (286.0, 510.0));

        // Landscape 1280x720 stream inside 760x510 tile must fit width cleanly.
        let (lw, lh) = fit_image_contain(760.0, 510.0, 1280.0, 720.0);
        assert_eq!((lw, lh), (760.0, 427.0));
    }

    #[test]
    fn sanitized_call_json_expands_nested_json_and_masks_sensitive_fields() {
        let frame = serde_json::json!({
            "act_type": "voip",
            "act": "request",
            "data": {
                "uidN": "1000000000000000003",
                "port": 40000,
                "ip": "203.0.113.5",
                "error_code": 0,
                "params": "{\"Dname\":\"Alice\",\"extendData\":\"{\\\"callType\\\":0,\\\"enableA2V\\\":\\\"1\\\"}\"}",
                "token": "secret-abc"
            }
        });
        let out = sanitized_call_json(&frame);
        assert!(out.contains("act_type:voip"));
        assert!(out.contains("act:request"));
        assert!(out.contains("error_code:0"));
        assert!(out.contains("callType:0"));
        assert!(out.contains("enableA2V:1"));
        assert!(!out.contains("1000000000000000003"));
        assert!(!out.contains("40000"));
        assert!(!out.contains("203.0.113.5"));
        assert!(!out.contains("Alice"));
        assert!(!out.contains("secret-abc"));
    }

    #[test]
    fn resolve_camera_selection_accepts_labels_paths_and_default() {
        assert_eq!(resolve_camera_selection("").as_deref(), None);
        assert_eq!(resolve_camera_selection("Mặc định").as_deref(), None);
        assert_eq!(
            resolve_camera_selection("/dev/video0").as_deref(),
            Some("/dev/video0")
        );
        assert_eq!(
            resolve_camera_selection("/dev/video2 — Logitech C920").as_deref(),
            Some("/dev/video2")
        );
    }

    #[test]
    fn incoming_call_guard_respects_call_prefs() {
        assert!(should_ring_incoming(true, false, false, false, false));
        assert!(!should_ring_incoming(false, false, false, true, true));
        assert!(!should_ring_incoming(true, true, false, false, true));
        assert!(should_ring_incoming(true, true, false, true, false));
        assert!(!should_ring_incoming(true, false, true, true, false));
        assert!(should_ring_incoming(true, true, true, true, true));
    }

    #[test]
    fn call_bar_state_matches_call_js_visibility() {
        let voice_ring = call_bar_state(false, false, false, false, false, false);
        assert!(!voice_ring.show_screen);
        assert!(!voice_ring.show_full);
        assert!(!voice_ring.show_layout);
        assert!(voice_ring.cam_disabled);

        let vid_up = call_bar_state(true, false, true, true, false, false);
        assert!(vid_up.show_screen);
        assert!(vid_up.show_full);
        assert!(vid_up.show_layout);
        assert!(!vid_up.cam_disabled);

        let vid_share = call_bar_state(true, false, true, true, true, false);
        assert!(vid_share.show_screen);
        assert!(vid_share.show_layout);
        assert!(vid_share.cam_disabled);

        let grp_up = call_bar_state(true, true, true, false, false, false);
        assert!(grp_up.show_screen);
        assert!(grp_up.show_full);
        assert!(grp_up.show_layout);
        assert!(!grp_up.cam_disabled);

        let ended = call_bar_state(true, true, true, true, false, true);
        assert!(!ended.show_screen);
        assert!(!ended.show_full);
        assert!(!ended.show_layout);
    }

    #[test]
    fn group_grid_and_conference_layouts_match_zcall_geometry() {
        let g3 = group_grid_layout(760.0, 430.0, 3);
        assert_eq!(g3.len(), 3);
        // Bottom row has 1 centered tile horizontally centered around x ~= (760 - w) / 2.
        assert!((g3[2].x - (760.0 - g3[2].w) * 0.5).abs() <= 1.0);
        assert_eq!(g3[0].w, g3[2].w);

        let conf = group_conference_layout(1000.0, 600.0, 4, 2);
        assert_eq!(conf.len(), 4);
        assert_eq!(conf[2].x, 0.0);
        assert_eq!(conf[2].h, 600.0);
        assert!(conf[2].w > 700.0);
        assert_eq!(conf[0].w, 219.0);
        assert!(conf[0].x > conf[2].w);
    }

    #[test]
    fn parse_member_list_accepts_engine_payload() {
        let parsed = parse_member_list(&serde_json::json!([
            {
                "src": "133222413",
                "name": "Alice",
                "avatar": "https://example.test/a.jpg",
                "muted": true,
                "camOff": false,
                "state": "incall"
            },
            {
                "src": 42,
                "name": "Bob",
                "muted": false,
                "camOff": true,
                "state": "ringing"
            }
        ]));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].uid, 133222413);
        assert!(parsed[0].muted);
        assert_eq!(parsed[1].uid, 42);
        assert!(parsed[1].cam_off);
        assert_eq!(parsed[1].state, "ringing");
    }

    #[test]
    fn mic_dots_behave_as_expected() {
        assert_eq!(super::mic_level_to_dots(127, false), 0);
        assert_eq!(super::mic_level_to_dots(20, true), 0);
        assert_eq!(super::mic_level_to_dots(10, false), 16);
        let mid = super::mic_level_to_dots(40, false);
        assert!((6..=11).contains(&mid), "expected mid-range dots, got {mid}");
    }
}
