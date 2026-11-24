//! FaceTime / AVFoundation camera via `ffmpeg`, encoded with OpenH264.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Level,
    Profile, QpRange, RateControlMode, SpsPpsStrategy, UsageType,
};
use openh264::formats::YUVBuffer;
use openh264::{OpenH264API, Timestamp};
use zcall_core::send_rate::Profile as SendProfile;

use crate::macos_media::{self, bgra_to_i420, downscale_bgra};

/// One encoded access unit, or why capture stopped.
pub(crate) enum CamEvent {
    /// Annex-B. `key` is an IDR.
    Frame {
        /// This access unit is a key frame.
        key: bool,
        /// Annex-B bytes.
        data: Vec<u8>,
    },
    /// Capture cannot continue.
    Failed(String),
}

/// Uncompressed local preview.
pub(crate) struct LocalPreview {
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
    /// Packed BGRA.
    pub bgra: Arc<Vec<u8>>,
}

enum CamCtrl {
    Profile(SendProfile),
    ForceKey,
}

/// AVFoundation camera encoded on a background thread.
pub(crate) struct CameraCapture {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<CamEvent>,
    ctrl_tx: Sender<CamCtrl>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
}

impl CameraCapture {
    /// Starts `ffmpeg -f avfoundation`. A missing camera is reported as [`CamEvent::Failed`].
    ///
    /// # Errors
    ///
    /// Returns when the thread cannot be spawned.
    pub(crate) fn start(
        initial: SendProfile,
        preferred_device: Option<String>,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let preview = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::sync_channel(3);
        let (ctrl_tx, ctrl_rx) = mpsc::channel();
        let stop_t = Arc::clone(&stop);
        let child_t = Arc::clone(&child);
        let preview_t = Arc::clone(&preview);
        let handle = std::thread::Builder::new()
            .name("zcall-cam".into())
            .spawn(move || {
                capture_loop(stop_t, child_t, tx, ctrl_rx, initial, preferred_device, preview_t);
            })
            .map_err(|err| format!("Không mở được camera: {err}"))?;
        Ok(Self {
            stop,
            child,
            handle: Some(handle),
            rx,
            ctrl_tx,
            preview,
        })
    }

    /// Latest local preview.
    pub(crate) fn take_preview(&self) -> Option<LocalPreview> {
        self.preview.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Updates the encode profile. Size changes restart `ffmpeg`.
    pub(crate) fn set_profile(&self, profile: SendProfile) {
        let _ = self.ctrl_tx.send(CamCtrl::Profile(profile));
    }

    /// Requests an IDR on the next frame.
    pub(crate) fn request_key_frame(&self) {
        let _ = self.ctrl_tx.send(CamCtrl::ForceKey);
    }

    /// Next encoded frame, if any.
    pub(crate) fn poll(&self) -> Option<CamEvent> {
        self.rx.try_recv().ok()
    }

    /// Stops ffmpeg and joins the thread.
    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        kill_child(&self.child);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CameraCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn kill_child(slot: &Mutex<Option<Child>>) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(child) = guard.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        *guard = None;
    }
}

const ENCODER_OPTION_FRAME_RATE: i32 = 4;
const ENCODER_OPTION_BITRATE: i32 = 5;
const ENCODER_OPTION_MAX_BITRATE: i32 = 6;
const SPATIAL_LAYER_0: std::os::raw::c_int = 0;
const SPATIAL_LAYER_ALL: std::os::raw::c_int = 4;
const LOCAL_PREVIEW_INTERVAL: Duration = Duration::from_millis(66);

#[repr(C)]
struct OpenH264BitrateInfo {
    layer: std::os::raw::c_int,
    bitrate: std::os::raw::c_int,
}

fn update_encoder_rate(enc: &mut Encoder, bitrate_bps: u32, fps: u32) -> bool {
    let target_bps = bitrate_bps.clamp(80_000, 4_000_000) as std::os::raw::c_int;
    let max_bps = (bitrate_bps.saturating_mul(5) / 4).clamp(100_000, 5_000_000) as std::os::raw::c_int;
    let mut fps_val = fps.clamp(5, 30) as f32;
    let mut max_all = OpenH264BitrateInfo {
        layer: SPATIAL_LAYER_ALL,
        bitrate: max_bps,
    };
    let mut max_l0 = OpenH264BitrateInfo {
        layer: SPATIAL_LAYER_0,
        bitrate: max_bps,
    };
    let mut tgt_all = OpenH264BitrateInfo {
        layer: SPATIAL_LAYER_ALL,
        bitrate: target_bps,
    };
    let mut tgt_l0 = OpenH264BitrateInfo {
        layer: SPATIAL_LAYER_0,
        bitrate: target_bps,
    };
    unsafe {
        let raw = enc.raw_api();
        let ok_max = raw.set_option(
            ENCODER_OPTION_MAX_BITRATE,
            std::ptr::addr_of_mut!(max_all).cast(),
        ) == 0;
        let _ = raw.set_option(
            ENCODER_OPTION_MAX_BITRATE,
            std::ptr::addr_of_mut!(max_l0).cast(),
        );
        let ok_tgt = raw.set_option(
            ENCODER_OPTION_BITRATE,
            std::ptr::addr_of_mut!(tgt_all).cast(),
        ) == 0;
        let _ = raw.set_option(
            ENCODER_OPTION_BITRATE,
            std::ptr::addr_of_mut!(tgt_l0).cast(),
        );
        let ok_fps = raw.set_option(
            ENCODER_OPTION_FRAME_RATE,
            std::ptr::addr_of_mut!(fps_val).cast(),
        ) == 0;
        ok_max && ok_tgt && ok_fps
    }
}

fn capture_loop(
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    tx: mpsc::SyncSender<CamEvent>,
    ctrl_rx: Receiver<CamCtrl>,
    mut profile: SendProfile,
    preferred: Option<String>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
) {
    let fail = |text: String| {
        let _ = tx.try_send(CamEvent::Failed(text));
    };
    if !macos_media::ffmpeg_available() {
        fail("Cần ffmpeg (brew install ffmpeg) để dùng camera.".into());
        return;
    }
    let device = preferred
        .as_deref()
        .and_then(parse_camera_path)
        .unwrap_or_else(|| "0".to_owned());
    let mut force_key = true;
    let mut encoder: Option<Encoder> = None;
    let mut enc_initialized = false;
    let mut enc_size = (0_usize, 0_usize);
    let mut enc_bitrate = 0_u32;
    let mut enc_fps = 0_u32;
    let started = Instant::now();
    let mut last_preview = Instant::now() - LOCAL_PREVIEW_INTERVAL;
    let mut next_due = Instant::now();

    // Open AVFoundation capture once at 720p30 (fallback 360p30) and scale
    // frames in-process so SendRate rung switches never kill/respawn ffmpeg.
    let attempts = [(1280_usize, 720_usize), (640_usize, 360_usize)];
    let mut opened: Option<(usize, usize, std::process::ChildStdout, Vec<u8>)> = None;
    for &(cw, ch) in &attempts {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut stdout) = spawn_ffmpeg(&device, cw, ch, 30, &child) else {
            continue;
        };
        let mut first = vec![0_u8; cw * ch * 4];
        if read_exact(&mut stdout, &mut first) {
            opened = Some((cw, ch, stdout, first));
            break;
        }
        kill_child(&child);
    }
    let Some((cap_w, cap_h, mut stdout, mut first_frame)) = opened else {
        fail(
            "Camera ngừng trả hình. Kiểm tra quyền Camera trong Cài đặt hệ thống.".into(),
        );
        return;
    };

    let mut bgra = vec![0_u8; cap_w * cap_h * 4];
    let mut have_first = true;
    while !stop.load(Ordering::Acquire) {
        while let Ok(ctrl) = ctrl_rx.try_recv() {
            match ctrl {
                CamCtrl::ForceKey => force_key = true,
                CamCtrl::Profile(next) => profile = next,
            }
        }
        if have_first {
            bgra.copy_from_slice(&first_frame);
            first_frame.clear();
            have_first = false;
        } else if !read_exact(&mut stdout, &mut bgra) {
            if !stop.load(Ordering::Acquire) {
                fail(
                    "Camera ngừng trả hình. Kiểm tra quyền Camera trong Cài đặt hệ thống.".into(),
                );
            }
            kill_child(&child);
            return;
        }

        let now = Instant::now();
        let target_fps = profile.fps.clamp(5, 30);
        let interval = Duration::from_secs_f64(1.0 / f64::from(target_fps));
        if !force_key && now + interval / 2 < next_due {
            continue;
        }
        next_due = (next_due + interval).max(now - interval / 2);

        let width = (even(profile.width).max(16) as usize).min(cap_w);
        let height = (even(profile.height).max(16) as usize).min(cap_h);
        let want_bitrate = profile.bitrate.max(150_000);
        if encoder.is_none() || enc_size != (width, height) {
            match open_encoder(width, height, want_bitrate, target_fps) {
                Ok(enc) => {
                    encoder = Some(enc);
                    enc_initialized = false;
                    enc_size = (width, height);
                    enc_bitrate = want_bitrate;
                    enc_fps = target_fps;
                    force_key = true;
                }
                Err(err) => {
                    fail(format!("Không mã hóa được hình: {err}"));
                    kill_child(&child);
                    return;
                }
            }
        } else if let Some(enc) = encoder.as_mut() {
            if (enc_bitrate != want_bitrate || enc_fps != target_fps)
                && enc_initialized
                && update_encoder_rate(enc, want_bitrate, target_fps)
            {
                enc_bitrate = want_bitrate;
                enc_fps = target_fps;
            }
        }

        if last_preview.elapsed() >= LOCAL_PREVIEW_INTERVAL {
            publish_preview(&preview, &bgra, cap_w, cap_h);
            last_preview = now;
        }
        let Some(enc) = encoder.as_mut() else {
            continue;
        };
        if force_key {
            enc.force_intra_frame();
            force_key = false;
        }
        let i420 = if (width, height) == (cap_w, cap_h) {
            bgra_to_i420(&bgra, cap_w, cap_h)
        } else {
            let scaled = downscale_bgra(&bgra, cap_w, cap_h, width, height);
            bgra_to_i420(&scaled, width, height)
        };
        let picture = YUVBuffer::from_vec(i420, width, height);
        let ts = Timestamp::from_millis(started.elapsed().as_millis() as u64);
        let Ok(bitstream) = enc.encode_at(&picture, ts) else {
            continue;
        };
        enc_initialized = true;
        if bitstream.frame_type() == FrameType::Skip {
            continue;
        }
        let data = bitstream.to_vec();
        if data.is_empty() {
            continue;
        }
        let key = bitstream.frame_type() == FrameType::IDR;
        if let Err(TrySendError::Disconnected(_)) = tx.try_send(CamEvent::Frame { key, data }) {
            kill_child(&child);
            return;
        }
    }
    kill_child(&child);
}

fn publish_preview(slot: &Mutex<Option<LocalPreview>>, bgra: &[u8], width: usize, height: usize) {
    let (pw, ph) = fit_edge(width, height, 960);
    let preview = downscale_bgra(bgra, width, height, pw, ph);
    if preview.is_empty() {
        return;
    }
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(LocalPreview {
            w: pw as u32,
            h: ph as u32,
            bgra: Arc::new(preview),
        });
    }
}

fn spawn_ffmpeg(
    device: &str,
    width: usize,
    height: usize,
    fps: u32,
    slot: &Mutex<Option<Child>>,
) -> Result<std::process::ChildStdout, String> {
    kill_child(slot);
    let fps = fps.clamp(5, 30).to_string();
    let input = format!("{device}:none");
    let scale = format!("scale={width}:{height}");
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-f",
            "avfoundation",
            "-framerate",
        ])
        .arg(fps)
        .arg("-i")
        .arg(input)
        .arg("-vf")
        .arg(scale)
        .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("Không chạy được ffmpeg: {err}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffmpeg không có stdout".to_owned())?;
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(child);
    }
    Ok(stdout)
}

fn open_encoder(width: usize, height: usize, bitrate: u32, fps: u32) -> Result<Encoder, String> {
    let api = OpenH264API::from_source();
    let level = if width > 640 || height > 480 {
        Level::Level_4_0
    } else {
        Level::Level_3_1
    };
    let qp = if bitrate <= 350_000 {
        QpRange::new(16, 42)
    } else if bitrate <= 750_000 {
        QpRange::new(14, 38)
    } else {
        QpRange::new(12, 36)
    };
    let config = EncoderConfig::new()
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(bitrate))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(0))
        .profile(Profile::Baseline)
        .level(level)
        .complexity(Complexity::Medium)
        .qp(qp)
        .sps_pps_strategy(SpsPpsStrategy::ConstantId)
        .max_slice_len(1060)
        .adaptive_quantization(true)
        .background_detection(true)
        .scene_change_detect(true)
        .skip_frames(false)
        .num_threads(if width * height > 640 * 360 { 2 } else { 1 });
    Encoder::with_api_config(api, config).map_err(|err| err.to_string())
}

fn read_exact(reader: &mut impl Read, buf: &mut [u8]) -> bool {
    let mut filled = 0;
    let deadline = Instant::now() + Duration::from_secs(4);
    while filled < buf.len() {
        if Instant::now() > deadline {
            return false;
        }
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return false,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    true
}

fn even(value: u32) -> u32 {
    let value = value.max(2);
    value - (value % 2)
}

fn fit_edge(w: usize, h: usize, max_edge: usize) -> (usize, usize) {
    if w <= max_edge && h <= max_edge {
        return (even(w as u32) as usize, even(h as u32) as usize);
    }
    let scale = max_edge as f32 / w.max(h) as f32;
    (
        even((w as f32 * scale) as u32) as usize,
        even((h as f32 * scale) as u32) as usize,
    )
}

/// `"0 — FaceTime"` → `"0"`. `"Mặc định"` selects the system default.
#[must_use]
pub(crate) fn parse_camera_path(choice: &str) -> Option<String> {
    let trimmed = choice.trim();
    if trimmed.is_empty() || trimmed == "Mặc định" {
        return None;
    }
    let index = trimmed.split(" — ").next().unwrap_or(trimmed).trim();
    if !index.is_empty() && index.chars().all(|ch| ch.is_ascii_digit()) {
        Some(index.to_owned())
    } else {
        None
    }
}

/// `"0 — FaceTime HD Camera"`.
#[must_use]
pub(crate) fn format_camera_option(index: &str, name: &str) -> String {
    format!("{index} — {name}")
}

/// Camera names for Settings. Screen-capture devices are omitted.
#[must_use]
pub(crate) fn camera_option_labels() -> Vec<String> {
    macos_media::list_video_devices()
        .into_iter()
        .filter(|device| !device.is_screen())
        .map(|device| format_camera_option(&device.index, &device.name))
        .collect()
}

/// Moves `preferred` to the front when it matches an index.
#[must_use]
pub(crate) fn order_video_devices(mut devices: Vec<String>, preferred: Option<&str>) -> Vec<String> {
    let Some(target) = preferred.and_then(parse_camera_path) else {
        return devices;
    };
    if let Some(pos) = devices.iter().position(|item| {
        parse_camera_path(item).as_deref() == Some(target.as_str()) || item == &target
    }) {
        let picked = devices.remove(pos);
        devices.insert(0, picked);
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_choice_keeps_the_avfoundation_index() {
        assert_eq!(parse_camera_path("Mặc định"), None);
        assert_eq!(
            parse_camera_path("0 — FaceTime HD Camera").as_deref(),
            Some("0")
        );
        let ordered = order_video_devices(
            vec!["1 — Desk".into(), "0 — FaceTime".into()],
            Some("0 — FaceTime"),
        );
        assert_eq!(ordered[0], "0 — FaceTime");
    }
}
