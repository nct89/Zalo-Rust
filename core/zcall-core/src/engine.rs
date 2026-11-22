//! Control plane of the native call engine, ported from the reference call
//! implementation (`engine-core.js`).
//!
//! The 1-1 machine covers incoming, answer, reject, caller connect, 416,
//! 408, 405/409, and the camera/mic 12006 signal. Group calls use the 12433
//! family (request, ring, answer, broadcast, ping, end). Screen-share
//! signalling is 12064 for 1-1 and 12044 for a group. Mute, speaker, mic
//! volume, early peer audio, audio faults, and the 15 s / 45 s setup timers
//! follow the JS engine. Encoding H.264 and capturing the screen stay outside
//! this crate: `send_video_frame` takes an access unit that is already encoded.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

/// The codec string Zalo's engine sends (JsonCpp + newline).
const CODEC: &str =
    "[{\"dynamicFptime\":0,\"frmPtime\":20,\"name\":\"opus/16000/1\",\"payload\":112}]\n";

/// Outgoing 1-1 call: give up if the peer never reaches ringing.
const SETUP_TIMEOUT_MS: u64 = 15_000;
/// Outgoing 1-1 call: give up if ringing is not answered.
const RING_TIMEOUT_MS: u64 = 45_000;
/// Zalo drops control events older than a minute.
const ENDED_CALL_TTL_MS: u64 = 60_000;
/// Cap on remembered ended call ids.
const MAX_ENDED_CALLS: usize = 256;
/// Minimum gap between local key-frame requests.
const KEY_FRAME_GAP_MS: u64 = 500;

/// Media backend, stubbed in tests and in headless mode.
pub trait Media {
    /// Our host addresses as P2P candidates.
    fn local_candidates(&self, public_addr: Option<&str>) -> Option<Vec<Value>> {
        let _ = public_addr;
        None
    }
    /// Starts media for a role.
    fn start(&mut self, role: &str, rtp_ser_ip: Option<&str>, opts: &Value) {
        let _ = (role, rtp_ser_ip, opts);
    }
    /// Prepares media; returns the chosen server (None in headless mode).
    fn prepare(&mut self, role: &str) -> Option<Value> {
        let _ = role;
        None
    }
    /// Configures the negotiated session.
    fn on_negotiated(&mut self, params: &Value) {
        let _ = params;
    }
    /// Stops media.
    fn stop(&mut self) {}
    /// Returns call statistics.
    fn stats(&self) -> Value {
        Value::Null
    }
    /// True when this backend opens a UDP socket. Observe-only backends stay false.
    fn sends_udp(&self) -> bool {
        false
    }
    /// Group camera on or off. The default ignores it.
    fn set_group_camera(&mut self, _on: bool) {}
    /// Starts a screen-share stream with this SSRC.
    fn start_share(&mut self, _share_id: u32) {}
    /// Stops the screen-share stream.
    fn stop_share(&mut self) {}
    /// Sends one already-encoded H.264 access unit. `data` is Annex-B.
    fn send_video_frame(&mut self, _key: bool, _data: &[u8], _screen: bool) {}
    /// Mutes or unmutes the microphone capture.
    fn set_mute(&mut self, _on: bool) {}
    /// Turns the speaker off (`true`) or back on.
    fn set_speaker(&mut self, _off: bool) {}
    /// Sets the microphone gain, 0..=100.
    fn set_mic_volume(&mut self, _percent: u8) {}
    /// Asks one sender for a key frame. `src` is the group member; `None` is 1-1.
    fn request_peer_key_frame(&mut self, _why: &str, _src: Option<u32>) {}
    /// Asks the group SFU to forward this member's video.
    fn watch_member(&mut self, _uid: u32) {}
}

/// One person in a group call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupMember {
    /// Display name.
    pub name: String,
    /// Avatar URL.
    pub avatar: String,
    /// Zalo id, when this row was created from the invite list.
    pub id: String,
    /// `calling`, `ringing`, `incall`, or `left`.
    pub state: String,
    /// Microphone muted.
    pub muted: bool,
    /// Camera off.
    pub cam_off: bool,
}

/// A group-call close scheduled for later, so the status line stays readable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredFinish {
    /// Unix millis when the call should close.
    pub at: u64,
    /// Finish status sent on 12446.
    pub status: i64,
    /// Window close text.
    pub text: String,
    /// Whether 12438 and 12446 are sent.
    pub signals: bool,
}

/// Call window / notification UI, stubbed in tests.
pub trait Ui {
    /// Opens the call window.
    fn open(&mut self, _info: &Value) {}
    /// Sets the status line.
    fn status(&mut self, _text: &str) {}
    /// Sets the status line and, when `since` is non-zero, the connected clock.
    fn status_at(&mut self, text: &str, since: u64) {
        let _ = since;
        self.status(text);
    }
    /// Closes the window.
    fn close(&mut self, _text: &str) {}
    /// Shows an incoming-call notice.
    fn incoming(&mut self, _info: &Value) {}
    /// Renames the peer (alias).
    fn rename(&mut self, _name: &str) {}
    /// Updates the peer's camera/mic/share state.
    fn peer(&mut self, _peer: &Value) {}
    /// Replaces the group-call member tiles.
    fn members(&mut self, _list: &Value) {}
    /// Shows an audio-device problem while the call stays up.
    fn notice(&mut self, _text: &str) {}
    /// Asks the local encoder for a key frame.
    fn request_key_frame(&mut self) {}
    /// Group: a member started or stopped talking.
    fn speaking(&mut self, _uid: u32, _on: bool) {}
    /// Reports the microphone source volume, 0..=100.
    fn mic_volume(&mut self, _percent: u8) {}
}

/// A no-op media backend.
#[derive(Default)]
pub struct NoopMedia;

impl Media for NoopMedia {}

/// A no-op UI.
#[derive(Default)]
pub struct NoopUi;

impl Ui for NoopUi {}

/// One active call.
#[derive(Clone, Debug, Default)]
pub struct Call {
    /// "caller" or "callee".
    pub role: String,
    /// Peer Zalo id (18-19 digits).
    pub peer_id: String,
    /// Peer media UID (9 digits).
    pub peer_uid: u32,
    /// Our media UID (9 digits).
    pub local_uid: u32,
    /// Peer display name.
    pub peer_name: String,
    /// Peer avatar URL.
    pub avatar: String,
    /// 1 voice, 3 video.
    pub call_type: u32,
    /// Call id (fits a signed 32-bit int).
    pub call_id: i64,
    /// Media session id.
    pub sess_id: String,
    /// The chosen server.
    pub server: Value,
    /// zrtc_config from the server.
    pub config: Value,
    /// Peer P2P candidates.
    pub p2p: Vec<Value>,
    /// Current state: calling/ringing/incoming/connected/ended.
    pub state: String,
    /// Millis when the call connected.
    pub started_at: u64,
    /// Whether we muted the mic.
    pub muted: bool,
    /// Whether the speaker is off.
    pub speaker_off: bool,
    /// Whether the camera is reported on.
    pub cam_reported: bool,
    /// Whether we answered with the camera off.
    pub camera_off: bool,
    /// Peer camera off state.
    pub peer_cam_off: bool,
    /// Peer muted state.
    pub peer_muted: bool,
    /// Peer screen-sharing state.
    pub peer_sharing: bool,
    /// Whether this is a group call.
    pub group: bool,
    /// Whether we are sharing our screen.
    pub sharing: bool,
    /// Host media UID.
    pub host_call: i64,
    /// Numeric group id from the server.
    pub group_id: i64,
    /// Group id string from `groupInfo`.
    pub group_id_str: String,
    /// Window title.
    pub title: String,
    /// Roster cap.
    pub max_users: u32,
    /// Invited Zalo ids.
    pub invited: Vec<String>,
    /// Invite rows, used to name the single invitee once its UID appears.
    pub invite_partners: Vec<GroupMember>,
    /// Members keyed by UID or, before a UID exists, by Zalo id.
    pub members: BTreeMap<String, GroupMember>,
    /// 12097 period, milliseconds.
    pub ping_ms: u64,
    /// Unix millis of the last 12097.
    pub last_ping_at: u64,
    /// Unix millis when the outgoing attempt started.
    pub created_at: u64,
    /// Hang up an unanswered outgoing group call after this many milliseconds. 0 disables it.
    pub ring_limit_ms: u64,
    /// Someone joined.
    pub answered: bool,
    /// Our camera is on. `videoState` on the wire is the inverse.
    pub camera_on: bool,
    /// Wire call type: 0 voice, 1 video.
    pub group_call_type: u32,
    /// Screen-share peer UID.
    pub share_screen_id: u32,
    /// UIDs we already greeted.
    pub greeted: BTreeSet<String>,
    /// Second greet, `(uid, due unix millis)`.
    pub greet_again: Vec<(String, u64)>,
    /// Last member-list JSON, so the window is not refreshed with the same tiles.
    pub members_key: String,
    /// Last status line.
    pub status_text: String,
    /// Close the call at this time.
    pub deferred_finish: Option<DeferredFinish>,
    /// Unix millis when an unanswered 1-1 attempt should end. 0 means none.
    pub deadline_at: u64,
    /// Unix millis of the last local key-frame request.
    pub key_asked_at: u64,
    /// Unix millis to repeat initial 12006 camera state after connect (2 s after connect).
    pub cam_retry_at: u64,
    /// Last audio-fault sentence already shown.
    pub audio_notice: String,
    /// Devices that failed to open: `mic`, `speaker`.
    pub audio_faults: BTreeSet<String>,
}

/// The engine's emitted frame (same shape as the JS engine).
pub type Frame = Value;

/// Control-plane engine.
pub struct EngineCore {
    /// Frames emitted so far (collected; a real engine hands them to a socket).
    pub frames: Vec<Frame>,
    /// The active call, if any.
    pub call: Option<Call>,
    /// Local user config.
    pub local: Value,
    /// Ended call ids, so a late control for a dead call is ignored.
    pub ended_calls: BTreeMap<i64, u64>,
    /// Media backend.
    pub media: Box<dyn Media>,
    /// Call window UI.
    pub ui: Box<dyn Ui>,
    /// Group calls are handled. `ZCALL_GROUP=0` turns this off in the binary.
    pub group_enabled: bool,
    /// Unix millis when an unsupported-group notice should close. 0 means none.
    pub notice_close_at: u64,
    /// Text used when that notice closes.
    pub notice_close_text: String,
}

impl EngineCore {
    /// Builds an engine with the given media and UI.
    pub fn new(media: Box<dyn Media>, ui: Box<dyn Ui>) -> Self {
        Self {
            frames: Vec::new(),
            call: None,
            local: json!({}),
            ended_calls: BTreeMap::new(),
            media,
            ui,
            group_enabled: true,
            notice_close_at: 0,
            notice_close_text: String::new(),
        }
    }

    /// Drives group ping, ring timeout, and deferred close.
    ///
    /// The JS engine uses `setInterval`. This process has one poll loop, so it
    /// calls `tick` about every 5 ms.
    pub fn tick(&mut self) {
        self.tick_at(now_ms());
    }

    /// Same as [`EngineCore::tick`] at a chosen clock, for tests.
    pub fn tick_at(&mut self, now: u64) {
        if self.call.is_none() && self.notice_close_at > 0 && now >= self.notice_close_at {
            let text = self.notice_close_text.clone();
            self.notice_close_at = 0;
            self.ui.close(&text);
            return;
        }
        let Some(current) = self.call.clone() else {
            return;
        };
        if let Some(finish) = current.deferred_finish.clone() {
            if now >= finish.at && current.state != "ended" {
                self._group_finish(finish.status, Some(finish.text.as_str()), finish.signals);
                return;
            }
        }
        let Some(current) = self.call.clone() else {
            return;
        };
        if current.group
            && current.role == "caller"
            && !current.answered
            && (current.state == "calling"
                || current.state == "connecting"
                || current.state == "ringing")
            && current.ring_limit_ms > 0
            && now.saturating_sub(current.created_at) >= current.ring_limit_ms
        {
            self._group_hangup("no-answer");
            return;
        }
        if !current.group
            && current.role == "caller"
            && (current.state == "calling" || current.state == "ringing")
            && current.deadline_at > 0
            && now >= current.deadline_at
        {
            let text = if current.state == "calling" {
                "Người nhận không liên lạc được"
            } else {
                "Người nhận không trả lời"
            };
            self._one_to_one_timeout(text);
            return;
        }
        let due = current.greet_again.iter().any(|(_, at)| now >= *at);
        if due {
            if let Some(call) = self.call.as_mut() {
                call.greet_again.retain(|(_, at)| now < *at);
            }
            if self.call.as_ref().is_some_and(|call| {
                call.group && (call.state == "ringing" || call.state == "connected")
            }) {
                self._group_broadcast();
            }
        }
        let Some(current) = self.call.clone() else {
            return;
        };
        if !current.group
            && current.state == "connected"
            && current.cam_retry_at > 0
            && now >= current.cam_retry_at
        {
            if let Some(call) = self.call.as_mut() {
                call.cam_retry_at = 0;
            }
            if current.call_type == 3 {
                self._cam_mic(None);
            }
        }
        let Some(current) = self.call.clone() else {
            return;
        };
        if current.group
            && (current.state == "ringing" || current.state == "connected")
            && current.ping_ms > 0
            && now.saturating_sub(current.last_ping_at) >= current.ping_ms
        {
            self._group_ping();
            if let Some(call) = self.call.as_mut() {
                call.last_ping_at = now;
            }
        }
    }

    /// Emits one engine -> zalo frame.
    pub fn emit(&mut self, frame: Frame) {
        self.frames.push(frame);
    }

    /// Consumes one Zalo -> engine frame.
    pub fn on_zalo_frame(&mut self, frame: &Value) {
        let frame_type = frame.get("type").and_then(Value::as_str).unwrap_or("");
        let command = frame.get("command");
        let data = frame.get("data").cloned().unwrap_or(Value::Null);
        match frame_type {
            "update" => self._on_update(cmd_str(command), &data),
            "request" => self._on_request(cmd_str(command), &data),
            "control" => {
                let act = data.get("act").and_then(Value::as_str).unwrap_or("");
                self._on_control(act, &data);
            }
            "recvSignal" => self._on_recv_signal(command, &data),
            "response" if cmd_str(command) == Some("getAliasName") => {
                self._on_alias_name(&data);
            }
            _ => {}
        }
    }

    /// Starts the engine (native-ready).
    pub fn start(&mut self) {
        self.emit(json!({"type": "update", "command": "native-ready", "data": {}}));
    }

    /// Callee picks up.
    pub fn answer(&mut self, cam_start_off: bool) {
        if self.call.as_ref().is_some_and(|call| call.group) {
            self.group_answer(cam_start_off);
            return;
        }
        let Some(mut c) = self.call.clone() else {
            return;
        };
        if c.role != "callee" || c.state != "incoming" {
            return;
        }
        if cam_start_off {
            c.camera_off = true;
        }
        c.state = "connected".to_owned();
        c.started_at = now_ms();
        self.call = Some(c.clone());
        self.emit(json!({"type": "request", "command": "action", "data": {"id": 5}}));
        // engine-core.js answer(): negotiate, InitZRTP, 402, then start media.
        self.media.on_negotiated(&json!({
            "muted": c.muted,
            "speakerOff": c.speaker_off,
            "servers": [c.server],
            "config": c.config,
            "srtpMode": c.config.get("srtpMode").cloned().unwrap_or(Value::Null),
            "sessId": c.sess_id,
            "callId": c.call_id,
            "role": c.role,
            "localUid": c.local_uid,
            "peerUid": c.peer_uid,
            "p2pCandidates": c.p2p,
        }));
        self.ui.status("Đang kết nối…");
        let best = self.media.prepare("callee");
        let public_addr = best
            .as_ref()
            .and_then(|b| b.get("publicAddr"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let p2p = self
            .media
            .local_candidates(public_addr.as_deref())
            .unwrap_or_default();
        let extend = self._extend_data(
            &c,
            &c.server,
            None,
            json!({"numServers": 0, "p2p": p2p, "sP2P": 1, "select2side": 1, "supportCallBusy": 0}),
        );
        self.emit(json!({
            "type": "sendSignal",
            "command": 402,
            "data": {
                "callId": c.call_id,
                "callerId": c.peer_id,
                "codec": CODEC,
                "extendData": serde_json::to_string(&extend).unwrap_or_else(|_| "{}\n".into()),
                "rtcpAddress": str_or(&c.server, "rtcpaddr"),
                "rtpAddress": str_or(&c.server, "rtpaddr"),
                "session": c.sess_id,
                "status": 0,
            }
        }));
        if best.is_none() && self.media.sends_udp() {
            self.ui.status("Không kết nối được máy chủ");
            self.hangup("no-server");
            return;
        }

        let rtp = str_or(&c.server, "rtpaddr");
        self.media
            .start(&c.role, Some(rtp.as_str()), &json!({"p2pCandidates": p2p}));
        self.ui.status_at("Đã kết nối", c.started_at);
        if c.camera_off {
            self._cam_mic(Some(false));
        } else if c.call_type == 3 {
            if let Some(call) = self.call.as_mut() {
                call.cam_retry_at = c.started_at.saturating_add(2_000);
            }
            self._cam_mic(Some(true));
        }
    }

    /// Callee declines.
    pub fn reject(&mut self, why: &str) {
        if self.call.as_ref().is_some_and(|call| call.group) {
            self._group_reject(why);
            return;
        }
        let Some(c) = self.call.clone() else {
            return;
        };
        if c.role != "callee" || c.state != "incoming" {
            return;
        }
        if why == "no-device" {
            self._finish(0, None, false);
            return;
        }
        self.emit(json!({"type": "request", "command": "action", "data": {"id": 8}}));
        self.emit(json!({
            "type": "sendSignal",
            "command": 402,
            "data": {
                "callId": c.call_id,
                "callerId": c.peer_id,
                "codec": "[]\n",
                "extendData": "{}\n",
                "rtcpAddress": "",
                "rtpAddress": "",
                "session": c.sess_id,
                "status": 3,
            }
        }));
        self._finish(160, None, true);
    }

    /// Local hangup.
    pub fn hangup(&mut self, why: &str) {
        let Some(c) = self.call.clone() else {
            return;
        };
        if c.state == "ended" {
            return;
        }
        if c.group {
            self._group_hangup(why);
            return;
        }
        if c.state == "incoming" {
            self.reject(why);
            return;
        }
        if c.state == "connected" {
            self.emit(json!({"type": "sendSignal", "command": 409, "data": {"callId": c.call_id, "toId": c.peer_id}}));
        } else {
            self.emit(json!({"type": "sendSignal", "command": 405, "data": {"callId": c.call_id, "callType": if c.call_type == 3 { 1 } else { 0 }, "status": 0, "toId": c.peer_id}}));
        }
        self._finish(if c.state == "connected" { 50 } else { 103 }, None, true);
    }

    /// Builds and emits 416 (caller), with our P2P candidates.
    pub fn _send416(&mut self, c: &Call, server: &Value, public_addr: Option<&str>) {
        let mut extend = self._extend_data(c, server, public_addr, json!({}));
        if let Some(p2p) = self.media.local_candidates(public_addr) {
            extend["p2p"] = json!(p2p);
            extend["sP2P"] = json!(1);
        }
        self.emit(json!({
            "type": "sendSignal",
            "command": 416,
            "data": {
                "callId": c.call_id,
                "calleeId": c.peer_id,
                "codec": CODEC,
                "extendData": serde_json::to_string(&extend).unwrap_or_else(|_| "{}\n".into()),
                "rtcpAddress": str_or(server, "rtcpaddr"),
                "rtpAddress": str_or(server, "rtpaddr"),
                "session": c.sess_id,
            }
        }));
    }

    /// Sets the camera reported state and sends 12006.
    pub fn _cam_mic(&mut self, cam_on: Option<bool>) {
        let Some(mut c) = self.call.clone() else {
            return;
        };
        if let Some(on) = cam_on {
            c.cam_reported = on;
        }
        let cam = if c.cam_reported { 1 } else { 0 };
        if c.group {
            c.camera_on = cam == 1;
            self.call = Some(c.clone());
            self.media.set_group_camera(c.camera_on);
            if c.state == "connected" {
                self._group_broadcast();
            }
            return;
        }
        if c.call_type != 3 && cam == 1 && c.state == "connected" {
            c.call_type = 3;
            self.emit(json!({
                "type": "sendSignal",
                "command": 12056,
                "data": {"callId": c.call_id, "calleeId": c.peer_id, "status": 1},
            }));
        }
        self.emit(json!({
            "type": "sendSignal",
            "command": 12006,
            "data": {"callId": c.call_id, "calleeId": c.peer_id, "data": "", "isOnCam": cam, "isOnMic": -1},
        }));
        self.call = Some(c);
    }

    fn _on_update(&mut self, command: Option<&str>, data: &Value) {
        if (command == Some("init") || command == Some("updateLocal"))
            && data.get("local").is_some()
        {
            if let Some(obj) = data.get("local").and_then(Value::as_object) {
                let mut merged = match self.local.clone() {
                    Value::Object(map) => map,
                    _ => serde_json::Map::new(),
                };
                for (k, v) in obj {
                    merged.insert(k.clone(), v.clone());
                }
                self.local = Value::Object(merged);
            }
        }
    }

    fn _on_request(&mut self, command: Option<&str>, data: &Value) {
        match command {
            Some("makeCall") => self._make_call(data),
            Some("endCall") => self.hangup("zalo"),
            _ => {}
        }
    }

    fn _make_call(&mut self, data: &Value) {
        if self.call.is_some() {
            return;
        }
        let call_type = data.get("type").and_then(Value::as_u64).unwrap_or(1);
        let group_info = data.get("groupInfo").filter(|value| value.is_object());
        if call_type == 5 || call_type == 6 || group_info.is_some() {
            if self.group_enabled {
                self._group_outgoing(data);
                return;
            }
            let group = group_info.cloned().unwrap_or_else(|| json!({}));
            let title = {
                let name = str_or(&group, "name");
                if name.is_empty() {
                    let alt = str_or(&group, "groupName");
                    if alt.is_empty() {
                        "Gọi nhóm".to_owned()
                    } else {
                        alt
                    }
                } else {
                    name
                }
            };
            self.emit(json!({"type": "response", "command": "show", "data": {"error": 0, "extendData": "success"}}));
            self.emit(json!({"type": "update", "command": "callState", "data": {"state": "free"}}));
            self._group_not_supported(&title, &str_or(&group, "avatar"));
            return;
        }
        let partner = data
            .get("partner")
            .and_then(Value::as_array)
            .and_then(|p| p.first())
            .cloned()
            .unwrap_or(json!({}));
        let call_type = data.get("type").and_then(Value::as_u64).unwrap_or(1) as u32;
        let call = Call {
            role: "caller".to_owned(),
            peer_id: str_or(&partner, "id"),
            peer_name: str_or(&partner, "name"),
            avatar: str_or(&partner, "avatar"),
            call_type,
            cam_reported: call_type == 3,
            call_id: new_call_id(),
            state: "calling".to_owned(),
            deadline_at: now_ms().saturating_add(SETUP_TIMEOUT_MS),
            ..Call::default()
        };
        let c = call.clone();
        self.call = Some(call);
        self.emit(json!({"type": "update", "command": "callState", "data": {"state": "incall"}}));
        self.emit(json!({"type": "response", "command": "show", "data": {"error": 0, "extendData": "success"}}));
        self.emit(json!({"type": "sendSignal", "command": 401, "data": {"callId": c.call_id, "calleeId": c.peer_id, "codec": "[]\n", "type": c.call_type}}));
        if c.call_type == 3 {
            self._cam_mic(None);
        }
        self.ui.open(&json!({
            "title": if c.peer_name.is_empty() { "Zalo" } else { c.peer_name.as_str() },
            "text": "Đang gọi…",
            "avatar": c.avatar,
            "video": c.call_type == 3,
            "selfAvatar": str_or(&self.local, "avatar"),
        }));
        if !c.peer_id.is_empty() {
            self.emit(json!({"type": "request", "command": "getAliasName", "data": {"noisedId": c.peer_id}}));
        }
    }

    fn _on_control(&mut self, act: &str, data: &Value) {
        let d = data.get("data").cloned().unwrap_or(json!({}));
        if act.starts_with("group_") {
            if self.group_enabled {
                self._on_group_control(act, &d);
            } else if act == "group_request" {
                self._incoming_group_notice(&d);
            } else {
                self.notice_close_at = 0;
                self.ui.close("Đã bỏ qua cuộc gọi nhóm");
            }
            return;
        }
        let call_id = call_id_field(&d).or_else(|| call_id_field(data));
        match act {
            "request" => self._incoming(&d),
            "ring_ring" => {
                if let Some(mut c) = self.call.clone() {
                    if c.state == "calling" {
                        c.state = "ringing".to_owned();
                        c.deadline_at = now_ms().saturating_add(RING_TIMEOUT_MS);
                        self.call = Some(c);
                        self.ui.status("Đang đổ chuông…");
                    }
                }
            }
            "answer" => {
                if let Some(id) = call_id {
                    self._remember_ended_call(id);
                }
                if let Some(c) = self.call.clone() {
                    if let Some(id) = call_id {
                        if id != c.call_id {
                            return;
                        }
                    }
                    if c.state == "incoming" {
                        self._finish(0, None, false);
                        return;
                    }
                }
                let Some(c) = self.call.clone() else { return };
                if c.role != "caller" || c.state == "connected" {
                    return;
                }
                let params = parse_maybe_json(d.get("params"));
                let status = num(&d, "status")
                    .or_else(|| num(&params, "status"))
                    .unwrap_or(0);
                if status != 0 {
                    self._finish(
                        if status == 3 { 160 } else { 103 },
                        Some(if status == 3 {
                            "Người nhận đã từ chối"
                        } else {
                            "Không liên lạc được"
                        }),
                        true,
                    );
                    return;
                }
                let ext = parse_maybe_json(params.get("extendData"));
                let rtp = str_or(&params, "rtpSerIp");
                let rtp = if rtp.is_empty() { None } else { Some(rtp) };
                let p2p = ext
                    .get("p2p")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                self._connect_caller(rtp.as_deref(), p2p);
            }
            "end_call" | "cancel" | "reject" | "busy" => {
                if let Some(id) = call_id {
                    self._remember_ended_call(id);
                }
                if let Some(c) = self.call.clone() {
                    if let Some(id) = call_id {
                        if id != c.call_id {
                            return;
                        }
                    }
                    if c.state == "incoming" {
                        self._finish(0, None, false);
                        return;
                    }
                }
                self._peer_ended(act);
            }
            "answer_ack" => {}
            "onoff_camera" | "mute_audio" | "share_screen" => {
                let Some(mut c) = self.call.clone() else {
                    return;
                };
                if let Some(id) = call_id_field(&d) {
                    if id != c.call_id {
                        return;
                    }
                }
                let p = parse_maybe_json(d.get("params"));
                let ty = num(&p, "type").or_else(|| num(&p, "status")).unwrap_or(0);
                if act == "onoff_camera" {
                    c.peer_cam_off = ty == 0;
                } else if act == "mute_audio" {
                    c.peer_muted = ty == 1;
                } else {
                    c.peer_sharing = ty == 1;
                }
                self.ui.peer(&json!({
                    "camOff": c.peer_cam_off,
                    "muted": c.peer_muted,
                    "sharing": c.peer_sharing,
                }));
                self.call = Some(c);
            }
            "requestaudio2video" | "audio2video" => {
                let Some(mut c) = self.call.clone() else {
                    return;
                };
                if let Some(id) = call_id_field(&d) {
                    if id != c.call_id {
                        return;
                    }
                }
                c.call_type = 3;
                c.peer_cam_off = false;
                self.ui.peer(&json!({
                    "camOff": c.peer_cam_off,
                    "muted": c.peer_muted,
                    "sharing": c.peer_sharing,
                }));
                self.call = Some(c);
            }
            _ => {}
        }
    }

    fn _on_recv_signal(&mut self, command: Option<&Value>, data: &Value) {
        let Some(c) = self.call.clone() else {
            return;
        };
        let cmd = command.map(value_to_string).unwrap_or_default();
        if cmd == "12433" && c.group {
            self._group_request_answered(data);
            return;
        }
        if cmd == "12434" && c.group {
            self._group_ring_result(data);
            return;
        }
        if cmd == "12044" && c.group {
            self._group_share_answered(data);
            return;
        }
        if cmd == "12064" || cmd == "12056" {
            return;
        }
        if cmd != "401" || c.state != "calling" || c.group {
            return;
        }
        let servers = data
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if servers.is_empty() {
            self._finish(103, None, true);
            return;
        }
        let mut c = c;
        c.sess_id = str_or(data, "sessId");
        c.local_uid = num(data, "fromId").unwrap_or(0) as u32;
        // The phone's zrtc_config (maxAudioDelayMs, …) has to stay on the call.
        // The worker reads it for the jitter ceiling; a missing copy left the
        // buffer on the 200 ms default.
        c.config = data.get("zrtc_config").cloned().unwrap_or(json!({}));
        let chosen = servers.first().cloned().unwrap_or(json!({}));
        self.media.on_negotiated(&json!({
            "muted": c.muted,
            "speakerOff": c.speaker_off,
            "servers": servers,
            "config": c.config.clone(),
            "srtpMode": c.config.get("srtpMode").cloned().unwrap_or(Value::Null),
            "sessId": c.sess_id,
            "callId": c.call_id,
            "role": c.role,
            "localUid": c.local_uid,
            "peerUid": num(data, "toId").unwrap_or(0),
        }));
        self.call = Some(c.clone());
        // engine-core.js: InitZRTP to every server first, then 416 with the
        // fastest one and our public address (the callee refused a bare 416
        // with answer status 5 — live test 2026-11-12).
        let best = self.media.prepare(&c.role);
        let Some(c) = self.call.clone() else {
            return;
        };
        if c.state == "ended" {
            return;
        }
        if best.is_none() && self.media.sends_udp() {
            self.ui.status("Không kết nối được máy chủ");
            self.hangup("no-server");
            return;
        }
        let public_addr = best
            .as_ref()
            .and_then(|b| b.get("publicAddr"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let server = best
            .as_ref()
            .and_then(|b| b.get("server"))
            .and_then(|srv| {
                let host = srv.get("host").and_then(Value::as_str)?;
                let port = srv.get("port").and_then(Value::as_u64)?;
                let addr = format!("{host}:{port}");
                servers
                    .iter()
                    .find(|raw| str_or(raw, "rtpaddr") == addr || str_or(raw, "rtcpaddr") == addr)
                    .cloned()
            })
            .unwrap_or(chosen);
        self._send416(&c, &server, public_addr.as_deref());
    }

    fn _connect_caller(&mut self, rtp_ser_ip: Option<&str>, p2p: Vec<Value>) {
        let Some(mut c) = self.call.clone() else {
            return;
        };
        if c.role != "caller" || c.state == "connected" || c.state == "ended" {
            return;
        }
        c.state = "connected".to_owned();
        c.started_at = now_ms();
        c.deadline_at = 0;
        if c.call_type == 3 {
            c.cam_retry_at = c.started_at.saturating_add(2_000);
        }
        self.call = Some(c.clone());
        let opts = json!({"p2pCandidates": p2p});
        self.media.start(&c.role, rtp_ser_ip, &opts);
        self.emit(json!({"type": "sendSignal", "command": 408, "data": {"callId": c.call_id, "calleeId": c.peer_id}}));
        if c.call_type == 3 {
            self._cam_mic(None);
        }
        self.ui.status_at("Đã kết nối", c.started_at);
    }

    fn _one_to_one_timeout(&mut self, text: &str) {
        let Some(c) = self.call.clone() else {
            return;
        };
        if c.group || c.state == "connected" || c.state == "ended" {
            return;
        }
        self.ui.status(text);
        self.emit(json!({
            "type": "sendSignal",
            "command": 405,
            "data": {
                "callId": c.call_id,
                "callType": if c.call_type == 3 { 1 } else { 0 },
                "status": 0,
                "toId": c.peer_id,
            }
        }));
        self._finish(103, Some(text), true);
    }

    /// Mutes the microphone and tells the peer (12006, or a group broadcast).
    pub fn mute(&mut self, on: bool) {
        self.media.set_mute(on);
        let Some(mut c) = self.call.clone() else {
            return;
        };
        c.muted = on;
        self.call = Some(c.clone());
        if c.group {
            self._group_broadcast();
        } else {
            self._mic_state(!on);
        }
    }

    /// `mic_on` is the opposite of muted: the phone shows the mic off when `isOnMic` is 1.
    fn _mic_state(&mut self, mic_on: bool) {
        let Some(c) = self.call.clone() else {
            return;
        };
        if c.state != "connected" || c.group {
            return;
        }
        self.emit(json!({
            "type": "sendSignal",
            "command": 12006,
            "data": {
                "callId": c.call_id,
                "calleeId": c.peer_id,
                "data": "",
                "isOnCam": -1,
                "isOnMic": if mic_on { 0 } else { 1 },
            }
        }));
    }

    /// Turns local playout off or on. The peer is not signalled.
    pub fn speaker(&mut self, off: bool) {
        if let Some(call) = self.call.as_mut() {
            call.speaker_off = off;
        }
        self.media.set_speaker(off);
    }

    /// Sets the microphone gain. Values outside 0..=100 are clamped.
    pub fn set_mic_volume(&mut self, percent: i64) {
        let pct = percent.clamp(0, 100) as u8;
        self.media.set_mic_volume(pct);
    }

    /// Peer RTP arrived before the answer control.
    pub fn early_audio(&mut self, phase: &str) {
        let Some(mut c) = self.call.clone() else {
            return;
        };
        if c.group || c.role != "caller" || c.state == "connected" || c.state == "ended" {
            return;
        }
        if phase == "first" && c.state == "calling" {
            c.state = "ringing".to_owned();
            c.deadline_at = now_ms().saturating_add(RING_TIMEOUT_MS);
            self.call = Some(c);
            self.ui.status("Đang đổ chuông…");
            return;
        }
        if phase == "stream" {
            self._connect_caller(None, Vec::new());
        }
    }

    /// The microphone, the speaker, or both stopped. The call stays up.
    pub fn audio_fault(&mut self, kind: &str) {
        let Some(mut c) = self.call.clone() else {
            return;
        };
        if c.state == "ended" {
            return;
        }
        if kind == "audio" {
            c.audio_faults.insert("mic".to_owned());
            c.audio_faults.insert("speaker".to_owned());
        } else {
            c.audio_faults.insert(kind.to_owned());
        }
        let mic = c.audio_faults.contains("mic");
        let speaker = c.audio_faults.contains("speaker");
        let text = if mic && speaker {
            "Không mở được micro và loa. Kiểm tra PipeWire / PulseAudio."
        } else if mic {
            "Không mở được micro, bên kia không nghe thấy bạn. Kiểm tra PipeWire / PulseAudio."
        } else {
            "Không phát được âm thanh ra loa. Kiểm tra PipeWire / PulseAudio."
        };
        if text == c.audio_notice {
            self.call = Some(c);
            return;
        }
        c.audio_notice = text.to_owned();
        self.call = Some(c);
        self.ui.notice(text);
    }

    /// The phone asked for a key frame. At most one request every 500 ms.
    pub fn request_key_frame(&mut self, why: &str) {
        let _ = why;
        let Some(mut c) = self.call.clone() else {
            return;
        };
        if c.state != "connected" {
            return;
        }
        let now = now_ms();
        if c.key_asked_at > 0 && now.saturating_sub(c.key_asked_at) < KEY_FRAME_GAP_MS {
            return;
        }
        c.key_asked_at = now;
        self.call = Some(c);
        self.ui.request_key_frame();
    }

    /// Our picture of that sender is damaged. The backend limits PLI to one a second.
    pub fn ask_peer_key(&mut self, why: &str, src: Option<u32>) {
        let Some(c) = self.call.as_ref() else {
            return;
        };
        if c.state != "connected" {
            return;
        }
        self.media.request_peer_key_frame(why, src);
    }

    fn _incoming(&mut self, d: &Value) {
        self._prune_ended_calls(now_ms());
        // The phone sends callId as a digit string (live log: callId:str10).
        // as_i64() drops that, the id becomes 0, and ring/answer/end come back
        // as error 114. Electron uses Number(d.callId).
        let call_id = call_id_field(d).unwrap_or(0);
        if self.ended_calls.contains_key(&call_id) || self.call.is_some() {
            return;
        }
        self.notice_close_at = 0;
        let params = parse_maybe_json(d.get("params"));
        let ext = parse_maybe_json(params.get("extendData"));
        let addr = ext
            .get("serverAddr")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(json!({}));
        let call = Call {
            role: "callee".to_owned(),
            peer_id: str_or(d, "uidN"),
            peer_uid: num(d, "uidFrom").unwrap_or(0) as u32,
            local_uid: num(d, "uidTo").unwrap_or(0) as u32,
            peer_name: str_or(&params, "Dname"),
            avatar: str_or(&params, "avatar"),
            call_type: if num(&ext, "callType") == Some(1) {
                3
            } else {
                1
            },
            call_id,
            sess_id: {
                let s = str_or(d, "session");
                if !s.is_empty() {
                    s
                } else {
                    str_or(&params, "sessId")
                }
            },
            server: json!({
                "rtpaddr": str_or(d, "rtpAddress"),
                "rtcpaddr": str_or(d, "rtcpAddress"),
                "rtpIPv6": str_or(&addr, "rtpIPv6"),
                "rtcpIPv6": str_or(&addr, "rtcpIPv6"),
            }),
            config: params.get("zrtc_config").cloned().unwrap_or(json!({})),
            p2p: ext
                .get("p2p")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            state: "incoming".to_owned(),
            ..Call::default()
        };
        let c = call.clone();
        self.call = Some(call);
        self.emit(json!({"type": "update", "command": "callState", "data": {"state": "incall"}}));
        self.emit(
            json!({"type": "request", "command": "getAliasName", "data": {"noisedId": c.peer_id}}),
        );
        self.emit(json!({"type": "sendSignal", "command": 407, "data": {"callId": c.call_id, "callerId": c.peer_id}}));
        let kind = if c.call_type == 3 {
            "Cuộc gọi video đến"
        } else {
            "Cuộc gọi thoại đến"
        };
        self.ui.incoming(&json!({
            "title": if c.peer_name.is_empty() { "Zalo" } else { c.peer_name.as_str() },
            "text": kind,
            "avatar": c.avatar,
            "video": c.call_type == 3,
            "selfAvatar": str_or(&self.local, "avatar"),
        }));
    }

    fn _on_alias_name(&mut self, d: &Value) {
        let Some(mut c) = self.call.clone() else {
            return;
        };
        let alias = str_or(d, "aliasName").trim().to_owned();
        if c.group || alias.is_empty() || str_or(d, "noisedId") != c.peer_id || alias == c.peer_name
        {
            return;
        }
        c.peer_name = alias.clone();
        self.ui.rename(&alias);
        self.call = Some(c);
    }

    fn _peer_ended(&mut self, why: &str) {
        let Some(c) = self.call.clone() else {
            return;
        };
        if c.state == "ended" {
            return;
        }
        if c.state == "connected" {
            self.emit(json!({"type": "request", "command": "showNotification", "data": {"isGroup": false, "reason": "partner_endcall", "toUid": c.peer_id, "userId": c.peer_id}}));
        }
        let code = if c.state == "connected" {
            50
        } else if why == "reject" {
            160
        } else {
            103
        };
        self._finish(code, None, true);
    }

    fn _extend_data(
        &self,
        c: &Call,
        server: &Value,
        public_addr: Option<&str>,
        extra: Value,
    ) -> Value {
        let mut p2p = Vec::new();
        if let Some(addr) = public_addr {
            if let Some((ip, port)) = addr.split_once('|') {
                p2p.push(json!({"ip": ip, "port": port.parse::<u32>().unwrap_or(0), "type": 1}));
            }
        }
        let mut ext = json!({
            "callType": if c.call_type == 3 { 1 } else { 0 },
            "fecTP": 0,
            "gccAudio": 1,
            "gccEarlyCall": 0,
            "gccMode": 1,
            "gccSVLR": 1,
            "maxFT": 60,
            "newZrtc": 1,
            "p2p": p2p,
            "packetMode": 2,
            "platform": 2,
            "sP2P": 0,
            "serverAddr": [{
                "rtcp": str_or(server, "rtcpaddr"),
                "rtcpIPv6": str_or(server, "rtcpIPv6"),
                "rtp": str_or(server, "rtpaddr"),
                "rtpIPv6": str_or(server, "rtpIPv6"),
                "tpType": 0,
            }],
            "spTcp": 1,
            "srtcp": 0,
            "srtpMode": 1,
            "supportCallBusy": 1,
            "supportHevcDecode": 0,
            "tpType": 0,
            "video": {"codec": [{"name": "h264", "payload": 97}]},
        });
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                ext[k] = v.clone();
            }
        }
        ext
    }

    fn _finish(&mut self, status: i64, close_text: Option<&str>, signals: bool) {
        let Some(mut c) = self.call.clone() else {
            return;
        };
        if c.state == "ended" {
            return;
        }
        self._remember_ended_call(c.call_id);
        let connected = c.state == "connected";
        c.state = "ended".to_owned();
        let duration = if connected {
            now_ms().saturating_sub(c.started_at) / 1000
        } else {
            0
        };
        let role = c.role == "caller";
        let stats = if c.sess_id.is_empty() {
            Value::Null
        } else {
            self.media.stats()
        };
        self.media.stop();
        if signals {
            self.emit(json!({"type": "update", "command": "bubble", "data": {"duration": duration, "partnerId": c.peer_id, "role": role}}));
            self.emit(json!({
                "type": "sendSignal",
                "command": 406,
                "data": {
                    "callId": c.call_id,
                    "data": format!("{{\"Codec\":[\"opus/16000/1\",\"20\"],\"NewZRTC\":1,\"RxTotalPkt\":{},\"SrtpMode\":1,\"TxTotalPkt\":{},\"sysInfo\":\"native-engine\"}}", stat_num(&stats, "rx"), stat_num(&stats, "tx")),
                    "duration": duration,
                    "partnerId": c.peer_id,
                    "role": role,
                    "status": status,
                }
            }));
        }
        self.emit(json!({"type": "update", "command": "callState", "data": {"state": "free"}}));
        let close = close_text.map(str::to_owned).unwrap_or_else(|| {
            if connected {
                format!("Kết thúc — {duration} s")
            } else {
                "Đã kết thúc".to_owned()
            }
        });
        self.ui.close(&close);
        self.call = None;
    }

    fn _remember_ended_call(&mut self, call_id: i64) {
        if call_id <= 0 {
            return;
        }
        let now = now_ms();
        self._prune_ended_calls(now);
        self.ended_calls.remove(&call_id);
        self.ended_calls.insert(call_id, now);
        while self.ended_calls.len() > MAX_ENDED_CALLS {
            let oldest = self
                .ended_calls
                .iter()
                .min_by_key(|(_, ended_at)| *ended_at)
                .map(|(id, _)| *id);
            let Some(oldest) = oldest else {
                break;
            };
            self.ended_calls.remove(&oldest);
        }
    }

    fn _prune_ended_calls(&mut self, now: u64) {
        let cutoff = now.saturating_sub(ENDED_CALL_TTL_MS);
        self.ended_calls.retain(|_, ended_at| *ended_at > cutoff);
    }

    /// 12044 answer. compact1913 stores `shareScreenId` and sends 12044 again
    /// with that id (`engine-core.js` case `'12044'`).
    fn _group_share_answered(&mut self, data: &Value) {
        let mut params = parse_maybe_json(data.get("data").or_else(|| data.get("params")));
        if params.get("params").is_some() {
            params = parse_maybe_json(params.get("params"));
        }
        let id = num(&params, "shareScreenId").unwrap_or(0) as u32;
        if id == 0 {
            return;
        }
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if !call.group {
            return;
        }
        let prev = call.share_screen_id;
        call.share_screen_id = id;
        let sharing = call.sharing;
        self.call = Some(call);
        if sharing && prev != id {
            self.screen_share(true);
        }
    }

    /// Tells the peer we started or stopped sharing the screen.
    pub fn screen_share(&mut self, on: bool) {
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if call.state != "connected" {
            return;
        }
        call.sharing = on;
        self.call = Some(call.clone());
        if call.group {
            if !on {
                self.media.stop_share();
            }
            if on && call.share_screen_id > 0 {
                self.media.start_share(call.share_screen_id);
            }
            let state = self._group_state(&call);
            self.emit(json!({
                "type": "sendSignal",
                "command": 12044,
                "data": {
                    "audioState": state.get("audioState").cloned().unwrap_or(json!(0)),
                    "callId": call.call_id,
                    "data": json_cpp(&json!({"shareScreenId": call.share_screen_id})),
                    "hostCall": call.host_call,
                    "status": if on { 1 } else { 0 },
                    "userId": call.local_uid,
                    "videoState": state.get("videoState").cloned().unwrap_or(json!(1)),
                }
            }));
            return;
        }
        self.emit(json!({
            "type": "sendSignal",
            "command": 12064,
            "data": {
                "callId": call.call_id,
                "status": if on { 1 } else { 0 },
                "uidTo": call.peer_id,
            }
        }));
    }

    /// Callee accepts a group call.
    pub fn group_answer(&mut self, cam_start_off: bool) {
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if !call.group || call.state != "incoming" {
            return;
        }
        call.camera_on = call.group_call_type == 1 && !cam_start_off;
        call.cam_reported = call.camera_on;
        call.state = "connecting".to_owned();
        self.call = Some(call.clone());
        self.media.set_group_camera(call.camera_on);
        let servers = call
            .server
            .get("servers")
            .cloned()
            .unwrap_or_else(|| json!([call.server.clone()]));
        self.media.on_negotiated(&json!({
            "muted": call.muted,
            "speakerOff": call.speaker_off,
            "group": true,
            "host": false,
            "servers": servers,
            "config": call.config,
            "srtpMode": call.config.get("srtpMode").cloned().unwrap_or(Value::Null),
            "sessId": call.sess_id,
            "callId": call.call_id,
            "role": "callee",
            "localUid": call.local_uid,
            "hostCall": call.host_call,
            "video": call.camera_on,
        }));
        self.ui.status("Đang kết nối…");
        let best = self.media.prepare("callee");
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if call.state != "connecting" {
            return;
        }
        if best.is_none() && self.media.sends_udp() {
            call.state = "failed".to_owned();
            self.call = Some(call.clone());
            self.ui
                .status("Không vào được cuộc gọi nhóm (máy chủ từ chối)");
            self._group_send_answer(&call, 3);
            self._schedule_group_finish(0, "Không vào được cuộc gọi nhóm", false, 3_000);
            return;
        }
        self.media.start("callee", None, &json!({}));
        call.state = "connected".to_owned();
        call.started_at = now_ms();
        call.last_ping_at = call.started_at;
        self.call = Some(call.clone());
        self._watch_group_members();
        self._group_send_answer(&call, 0);
        self._group_broadcast();
        self._group_status();
    }

    fn _group_not_supported(&mut self, title: &str, avatar: &str) {
        let text = "Gọi nhóm chưa được hỗ trợ trong phiên bản này";
        self.ui
            .open(&json!({"title": title, "text": text, "avatar": avatar}));
        self.notice_close_text = text.to_owned();
        self.notice_close_at = now_ms() + 3_000;
    }

    fn _incoming_group_notice(&mut self, data: &Value) {
        if self.call.is_some() {
            return;
        }
        let name = str_or(data, "Dname");
        let who = if name.is_empty() {
            "Ai đó"
        } else {
            name.as_str()
        };
        let title = {
            let group_name = str_or(data, "groupName");
            if group_name.is_empty() {
                "Cuộc gọi nhóm".to_owned()
            } else {
                group_name
            }
        };
        let avatar = {
            let group_avatar = str_or(data, "groupAvatar");
            if group_avatar.is_empty() {
                str_or(data, "avatar")
            } else {
                group_avatar
            }
        };
        self.ui.incoming(&json!({
            "title": title,
            "text": format!("{who} đang gọi nhóm. Gọi nhóm chưa được hỗ trợ trong phiên bản này."),
            "avatar": avatar,
            "noAnswer": true,
        }));
        self.notice_close_text = "Đã bỏ qua cuộc gọi nhóm".to_owned();
        self.notice_close_at = now_ms() + 45_000;
    }

    fn _on_group_control(&mut self, act: &str, data: &Value) {
        if act == "group_request" {
            self._group_incoming(data);
            return;
        }
        let Some(call) = self.call.clone() else {
            return;
        };
        if !call.group {
            return;
        }
        if let Some(id) = data
            .get("id")
            .and_then(json_i64)
            .or_else(|| call_id_field(data))
        {
            if id != call.call_id {
                return;
            }
        }
        let who = num(data, "userId")
            .or_else(|| num(data, "fromId"))
            .unwrap_or(0);
        match act {
            "group_broadcast" => {
                if who != 0 && who as u32 != call.local_uid {
                    let state = if num(data, "callState") == Some(3) {
                        "incall"
                    } else {
                        "ringing"
                    };
                    self._group_member(
                        who as u32,
                        state,
                        Some(num(data, "audioState") == Some(1)),
                        Some(num(data, "videoState") == Some(1)),
                    );
                }
            }
            "group_answer" => {
                if call.state == "incoming" && (who == 0 || who as u32 == call.local_uid) {
                    self._group_finish(0, Some("Cuộc gọi nhóm đã kết thúc"), false);
                    return;
                }
                if who != 0 && who as u32 != call.local_uid {
                    self._group_member(who as u32, "incall", None, None);
                    self._group_greet(who as u32);
                }
            }
            "group_ring_ring" => {
                if who != 0 && who as u32 != call.local_uid {
                    let state = self
                        .call
                        .as_ref()
                        .and_then(|c| c.members.get(&who.to_string()))
                        .map(|m| m.state.as_str())
                        .unwrap_or("");
                    if state != "incall" {
                        self._group_member(who as u32, "ringing", None, None);
                    }
                }
            }
            "group_end_call" | "group_cancel" => {
                if call.state == "incoming"
                    && (who == 0 || who == call.host_call || who as u32 == call.local_uid)
                {
                    self._group_finish(0, Some("Cuộc gọi nhóm đã kết thúc"), false);
                    return;
                }
                if who as u32 == call.local_uid {
                    let status = if call.state == "connected" { 50 } else { 0 };
                    self._group_finish(status, Some("Cuộc gọi nhóm đã kết thúc"), false);
                    return;
                }
                if who != 0 {
                    self._group_member_left(who as u32, act);
                }
            }
            _ => {}
        }
    }

    fn _group_outgoing(&mut self, data: &Value) {
        let group = data.get("groupInfo").cloned().unwrap_or_else(|| json!({}));
        let partners = match data.get("partner") {
            Some(Value::Array(list)) => list.clone(),
            Some(one) if one.is_object() => vec![one.clone()],
            _ => Vec::new(),
        };
        let ids: Vec<String> = partners
            .iter()
            .map(|p| str_or(p, "id"))
            .filter(|id| !id.is_empty())
            .collect();
        let mut members = BTreeMap::new();
        for (index, partner) in partners.iter().enumerate() {
            let id = str_or(partner, "id");
            let key = if id.is_empty() {
                (index + 1).to_string()
            } else {
                id.clone()
            };
            members.insert(
                key,
                GroupMember {
                    name: str_or(partner, "name"),
                    avatar: str_or(partner, "avatar"),
                    id,
                    state: "calling".to_owned(),
                    muted: false,
                    cam_off: true,
                },
            );
        }
        let wire = if data.get("type").and_then(Value::as_u64) == Some(6) {
            1
        } else {
            0
        };
        let title = {
            let name = str_or(&group, "name");
            if !name.is_empty() {
                name
            } else {
                let alt = str_or(&group, "groupName");
                if alt.is_empty() {
                    "Cuộc gọi nhóm".to_owned()
                } else {
                    alt
                }
            }
        };
        let call = Call {
            group: true,
            role: "caller".to_owned(),
            state: "calling".to_owned(),
            call_id: new_call_id(),
            group_call_type: wire,
            call_type: if wire == 1 { 3 } else { 1 },
            camera_on: wire == 1,
            cam_reported: wire == 1,
            group_id_str: str_or(&group, "id"),
            title: title.clone(),
            avatar: str_or(&group, "avatar"),
            max_users: num(&group, "maxUsers").unwrap_or(8) as u32,
            invited: ids.clone(),
            invite_partners: partners
                .iter()
                .map(|p| GroupMember {
                    name: str_or(p, "name"),
                    avatar: str_or(p, "avatar"),
                    id: str_or(p, "id"),
                    ..GroupMember::default()
                })
                .collect(),
            members,
            ping_ms: 9_000,
            created_at: now_ms(),
            ring_limit_ms: 60_000,
            ..Call::default()
        };
        let call_id = call.call_id;
        let avatar = call.avatar.clone();
        let group_id = call.group_id_str.clone();
        let max_users = call.max_users;
        self.call = Some(call);
        self.emit(json!({"type": "update", "command": "callState", "data": {"state": "incall"}}));
        self.emit(json!({"type": "response", "command": "show", "data": {"error": 0, "extendData": "success"}}));
        self.emit(json!({
            "type": "sendSignal",
            "command": 12433,
            "data": {
                "callId": call_id,
                "data": json_cpp(&json!({
                    "extraData": "",
                    "groupAvatar": avatar,
                    "groupId": group_id,
                    "groupName": title,
                    "maxUsers": max_users,
                    "noiseId": json_cpp(&json!(ids)),
                })),
                "groupId": group_id,
                "partners": json_cpp(&json!(ids)),
                "typeRequest": 1,
            }
        }));
        self.ui.open(&json!({
            "title": title,
            "text": "Đang gọi nhóm…",
            "avatar": avatar,
            "video": true,
            "group": true,
            "selfAvatar": str_or(&self.local, "avatar"),
        }));
        self._group_members_to_ui();
    }

    fn _group_request_answered(&mut self, data: &Value) {
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if !call.group {
            return;
        }
        let parsed = merge_signal_body(data);
        let setting = parse_maybe_json(parsed.get("callSetting"));
        let status = num(&parsed, "status")
            .or_else(|| num(data, "status"))
            .unwrap_or(0);
        call.host_call = num(data, "hostCall")
            .or_else(|| num(&parsed, "hostCall"))
            .unwrap_or(0);
        call.local_uid = call.host_call as u32;
        if let Some(id) = num(&parsed, "callId") {
            call.call_id = id;
        }
        if let Some(id) = num(&parsed, "groupId") {
            if id > 0 && id < 0x7fff_ffff {
                call.group_id = id;
            }
        }
        if let Some(interval) = num(&parsed, "interval") {
            call.ping_ms = if interval < 1000 {
                interval as u64 * 1000
            } else {
                interval as u64
            };
        }
        call.sess_id = {
            let from_setting = str_or(&setting, "session");
            if from_setting.is_empty() {
                str_or(&parsed, "session")
            } else {
                from_setting
            }
        };
        call.config = parse_maybe_json(setting.get("zrtcConfig"));
        let servers = setting
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        call.server = json!({"servers": servers});
        let message = str_or(&parsed, "msg");
        if status != 0 || call.sess_id.is_empty() || servers.is_empty() || call.local_uid == 0 {
            self.call = Some(call);
            let text = if message.is_empty() {
                "Không gọi được nhóm".to_owned()
            } else {
                format!("Không gọi được: {message}")
            };
            self.ui.status(&text);
            self._schedule_group_finish(0, "Không gọi được nhóm", false, 3_000);
            return;
        }
        call.state = "connecting".to_owned();
        self.call = Some(call.clone());
        self.media.on_negotiated(&json!({
            "muted": call.muted,
            "speakerOff": call.speaker_off,
            "group": true,
            "host": true,
            "servers": servers,
            "config": call.config,
            "srtpMode": call.config.get("srtpMode").cloned().unwrap_or(Value::Null),
            "sessId": call.sess_id,
            "callId": call.call_id,
            "role": "caller",
            "localUid": call.local_uid,
            "hostCall": call.host_call,
            "video": call.camera_on,
        }));
        let best = self.media.prepare("caller");
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if call.state != "connecting" {
            return;
        }
        if best.is_none() && self.media.sends_udp() {
            self.ui
                .status("Không vào được cuộc gọi nhóm (máy chủ từ chối)");
            self._schedule_group_finish(0, "Không vào được cuộc gọi nhóm", false, 3_000);
            return;
        }
        self.media.start("caller", None, &json!({}));
        // The room is up, but nobody has joined. Stay on "ringing" so the
        // window shows the "Ringing..." status and the clock stays at zero.
        // 12434 is what rings the selected members.
        call.state = "ringing".to_owned();
        call.last_ping_at = now_ms();
        self.call = Some(call.clone());
        self._watch_group_members();
        let server = server_for_best(&servers, best.as_ref());
        let rtcp_v6 = first_nonempty(&server, "rtcpaddrIPv6", "rtcpIPv6");
        let rtp_v6 = first_nonempty(&server, "rtpaddrIPv6", "rtpIPv6");
        let payload = json_cpp(&json!({
            "codec": "",
            "data": json_cpp(&json!({
                "groupAvatar": call.avatar,
                "groupName": call.title,
                "hostCall": call.host_call,
                "maxUsers": call.max_users,
                "noiseId": call.invited,
            })),
            "extendData": "",
            "rtcpAddress": str_or(&server, "rtcpaddr"),
            "rtcpAddressIPv6": rtcp_v6,
            "rtpAddress": str_or(&server, "rtpaddr"),
            "rtpAddressIPv6": rtp_v6,
        }));
        let callee_id = call.invited.first().cloned().unwrap_or_default();
        if callee_id.is_empty() {
            let text = "Không đổ chuông được tới thành viên".to_owned();
            if let Some(current) = self.call.as_mut() {
                current.status_text = text.clone();
            }
            self.ui.status(&text);
            self._schedule_group_finish(0, &text, false, 3_000);
            return;
        }
        let signal = json!({
            "type": "sendSignal",
            "command": 12434,
            "data": {
                "calleeId": callee_id,
                "callId": call.call_id,
                "callType": call.group_call_type,
                "data": payload,
                "groupId": call.group_id_str,
                "partners": json_cpp(&Value::Array(call.invited.iter().cloned().map(Value::String).collect())),
                "session": call.sess_id,
            }
        });
        self.call = Some(call);
        self.emit(signal);
        self._group_broadcast();
        self._group_status();
    }

    /// 12434 reply. Status other than 0 means the selected members were not rung.
    fn _group_ring_result(&mut self, data: &Value) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if !call.group || call.role != "caller" {
            return;
        }
        if call.state != "ringing" && call.state != "connecting" {
            return;
        }
        let parsed = merge_signal_body(data);
        let status = num(&parsed, "status")
            .or_else(|| num(data, "status"))
            .unwrap_or(0);
        if status == 0 {
            return;
        }
        let text = format!("Không đổ chuông được tới thành viên (mã {status})");
        if let Some(current) = self.call.as_mut() {
            current.status_text = text.clone();
        }
        self.ui.status(&text);
        self._schedule_group_finish(0, &text, false, 3_000);
    }

    fn _group_incoming(&mut self, data: &Value) {
        if self.call.is_some() {
            return;
        }
        self.notice_close_at = 0;
        let setting = parse_maybe_json(data.get("callSetting"));
        let local_uid = num(data, "receiverId").unwrap_or(0) as u32;
        let mut members = BTreeMap::new();
        if let Some(list) = data.get("partnerInfo").and_then(Value::as_array) {
            for partner in list {
                let uid = num(partner, "userId").unwrap_or(0) as u32;
                if uid == 0 || uid == local_uid {
                    continue;
                }
                members.insert(
                    uid.to_string(),
                    GroupMember {
                        name: str_or(partner, "name"),
                        avatar: str_or(partner, "avatar"),
                        state: if num(partner, "callState") == Some(3) {
                            "incall".to_owned()
                        } else {
                            "ringing".to_owned()
                        },
                        muted: num(partner, "audioState") == Some(1),
                        cam_off: num(partner, "videoState") == Some(1),
                        ..GroupMember::default()
                    },
                );
            }
        }
        let interval = num(data, "interval").unwrap_or(0);
        let ping_ms = if interval == 0 {
            10_000
        } else if interval < 1000 {
            interval as u64 * 1000
        } else {
            interval as u64
        };
        let servers = setting
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let sess_id = {
            let from_setting = str_or(&setting, "session");
            if from_setting.is_empty() {
                str_or(data, "session")
            } else {
                from_setting
            }
        };
        let title = {
            let name = str_or(data, "groupName");
            if name.is_empty() {
                "Cuộc gọi nhóm".to_owned()
            } else {
                name
            }
        };
        if sess_id.is_empty() || servers.is_empty() || local_uid == 0 {
            self._incoming_group_notice(data);
            return;
        }
        let host_call = num(data, "hostCall").unwrap_or(0);
        let call = Call {
            group: true,
            role: "callee".to_owned(),
            state: "incoming".to_owned(),
            call_id: num(data, "id").unwrap_or(0),
            host_call,
            group_id: num(data, "groupId").unwrap_or(0),
            local_uid,
            group_call_type: if num(data, "callType") == Some(1) {
                1
            } else {
                0
            },
            call_type: 3,
            sess_id: sess_id.clone(),
            server: json!({"servers": servers}),
            config: parse_maybe_json(setting.get("zrtcConfig")),
            title: title.clone(),
            avatar: str_or(data, "groupAvatar"),
            members,
            ping_ms,
            ..Call::default()
        };
        let host_name = call
            .members
            .get(&host_call.to_string())
            .map(|m| m.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                let name = str_or(data, "Dname");
                if name.is_empty() {
                    "Ai đó".to_owned()
                } else {
                    name
                }
            });
        self.call = Some(call.clone());
        self.emit(json!({"type": "update", "command": "callState", "data": {"state": "incall"}}));
        self.emit(json!({
            "type": "sendSignal",
            "command": 12439,
            "data": {
                "callId": call.call_id,
                "data": json_cpp(&json!({"callType": 1, "extraData": "", "status": 1})),
                "hostCall": call.host_call,
                "session": call.sess_id,
                "status": 1,
            }
        }));
        self.ui.incoming(&json!({
            "title": title,
            "text": format!("{host_name} mời bạn vào cuộc gọi nhóm"),
            "inviter": host_name,
            "avatar": call.avatar,
            "video": true,
            "group": true,
            "selfAvatar": str_or(&self.local, "avatar"),
        }));
    }

    fn _group_reject(&mut self, _why: &str) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if !call.group || call.state != "incoming" {
            return;
        }
        self._group_send_answer(&call, 3);
        self._group_finish(0, Some("Đã từ chối cuộc gọi nhóm"), false);
    }

    fn _group_send_answer(&mut self, call: &Call, status: i64) {
        self.emit(json!({
            "type": "sendSignal",
            "command": 12436,
            "data": {
                "callId": call.call_id,
                "data": json_cpp(&json!({
                    "callType": call.group_call_type,
                    "codec": "",
                    "extendData": "",
                    "groupId": call.group_id,
                    "status": status,
                })),
                "hostCall": call.host_call,
                "session": call.sess_id,
                "status": status,
            }
        }));
    }

    fn _group_state(&self, call: &Call) -> Value {
        json!({
            "audioState": if call.muted { 1 } else { 0 },
            "callId": call.call_id,
            "callState": 3,
            "hostCall": call.host_call,
            "userId": call.local_uid,
            "videoState": if call.camera_on { 0 } else { 1 },
        })
    }

    fn _group_greet(&mut self, uid: u32) {
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if !call.group
            || (call.state != "ringing" && call.state != "connected")
            || uid == call.share_screen_id
        {
            return;
        }
        let key = uid.to_string();
        if call.greeted.contains(&key) {
            return;
        }
        call.greeted.insert(key.clone());
        call.greet_again.push((key, now_ms() + 2_000));
        self.call = Some(call);
        self._group_broadcast();
    }

    fn _group_broadcast(&mut self) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if call.group && (call.state == "ringing" || call.state == "connected") {
            let state = self._group_state(&call);
            self.emit(json!({"type": "sendSignal", "command": 12098, "data": state}));
        }
    }

    fn _group_ping(&mut self) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if call.group && (call.state == "ringing" || call.state == "connected") {
            let state = self._group_state(&call);
            self.emit(json!({"type": "sendSignal", "command": 12097, "data": state}));
        }
    }

    fn _group_member(&mut self, uid: u32, state: &str, muted: Option<bool>, cam_off: Option<bool>) {
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if !call.group
            || uid == call.local_uid
            || (call.share_screen_id > 0 && uid == call.share_screen_id)
        {
            return;
        }
        let key = uid.to_string();
        let mut current = call.members.remove(&key);
        if current.is_none() && call.invite_partners.len() == 1 {
            let waiting = call
                .members
                .iter()
                .find(|(_, member)| member.state == "calling" || member.state == "ringing")
                .map(|(k, _)| k.clone());
            if let Some(waiting) = waiting {
                current = call.members.remove(&waiting);
            }
        }
        let mut next = current.unwrap_or_default();
        if !state.is_empty() {
            next.state = state.to_owned();
        }
        if let Some(muted) = muted {
            next.muted = muted;
        }
        if let Some(cam_off) = cam_off {
            next.cam_off = cam_off;
        }
        if next.name.is_empty() && call.invite_partners.len() == 1 {
            let partner = call.invite_partners[0].clone();
            next.name = partner.name;
            if next.avatar.is_empty() {
                next.avatar = partner.avatar;
            }
        }
        if state == "incall" {
            call.answered = true;
            call.ring_limit_ms = 0;
            if call.role == "caller" && call.state == "ringing" {
                call.state = "connected".to_owned();
                call.started_at = now_ms();
            }
        }
        call.members.insert(key, next);
        self.call = Some(call);
        if state == "incall" {
            self.media.watch_member(uid);
        }
        self._group_status();
    }

    /// Asks the SFU for video from everyone already in the room.
    fn _watch_group_members(&mut self) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if !call.group {
            return;
        }
        for (uid, member) in &call.members {
            if member.state != "incall" {
                continue;
            }
            let Ok(id) = uid.parse::<u32>() else {
                continue;
            };
            self.media.watch_member(id);
        }
    }

    fn _group_members_to_ui(&mut self) {
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if !call.group {
            return;
        }
        let mut list = Vec::new();
        for (uid, member) in &call.members {
            if member.state != "incall" && member.state != "ringing" && member.state != "calling" {
                continue;
            }
            if call.share_screen_id > 0 && uid == &call.share_screen_id.to_string() {
                continue;
            }
            list.push(json!({
                "src": uid,
                "name": member.name,
                "avatar": member.avatar,
                "muted": member.muted,
                "camOff": member.cam_off,
                "state": member.state,
            }));
        }
        let key = serde_json::to_string(&list).unwrap_or_default();
        if key == call.members_key {
            return;
        }
        call.members_key = key;
        self.call = Some(call);
        self.ui.members(&Value::Array(list));
    }

    /// Applies the server's room roster (command 50).
    pub fn group_roster(&mut self, uids: &[u32]) {
        self._group_roster(uids);
    }

    fn _group_roster(&mut self, uids: &[u32]) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if !call.group {
            return;
        }
        let in_room: BTreeSet<u32> = uids.iter().copied().collect();
        for uid in &in_room {
            if *uid == call.local_uid || (call.share_screen_id > 0 && *uid == call.share_screen_id)
            {
                continue;
            }
            let state = call
                .members
                .get(&uid.to_string())
                .map(|m| m.state.as_str())
                .unwrap_or("");
            if state != "incall" {
                self._group_greet(*uid);
            }
            self._group_member(*uid, "incall", None, None);
        }
        let Some(mut call) = self.call.clone() else {
            return;
        };
        for (uid, member) in call.members.iter_mut() {
            let Ok(id) = uid.parse::<u32>() else {
                continue;
            };
            if member.state == "incall" && !in_room.contains(&id) {
                member.state = "left".to_owned();
            }
        }
        self.call = Some(call);
        self._group_status();
        self._group_check_empty("roster");
    }

    /// A member left, reported by media or by `group_end_call`.
    pub fn group_member_left(&mut self, uid: u32, why: &str) {
        self._group_member_left(uid, why);
    }

    fn _group_member_left(&mut self, uid: u32, _why: &str) {
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if !call.group
            || uid == call.local_uid
            || (call.share_screen_id > 0 && uid == call.share_screen_id)
        {
            return;
        }
        let Some(member) = call.members.get_mut(&uid.to_string()) else {
            return;
        };
        if member.state == "left" {
            return;
        }
        member.state = "left".to_owned();
        self.call = Some(call);
        self._group_status();
        self._group_check_empty("left");
    }

    fn _group_check_empty(&mut self, _why: &str) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if !call.group || call.state != "connected" {
            return;
        }
        let others = call.members.values().any(|member| {
            member.state == "incall" || member.state == "ringing" || member.state == "calling"
        });
        if !others {
            self._group_hangup("alone");
        }
    }

    fn _group_status(&mut self) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if !call.group
            || (call.state != "connected" && call.state != "calling" && call.state != "ringing")
        {
            return;
        }
        self.call = Some(call.clone());
        self._group_members_to_ui();
        let Some(mut call) = self.call.clone() else {
            return;
        };
        let names: Vec<String> = call
            .members
            .iter()
            .filter(|(uid, member)| {
                member.state == "incall"
                    && (call.share_screen_id == 0
                        || uid.as_str() != call.share_screen_id.to_string())
            })
            .map(|(_, member)| {
                if member.name.is_empty() {
                    "Thành viên".to_owned()
                } else {
                    member.name.clone()
                }
            })
            .collect();
        let has_ringing = call
            .members
            .values()
            .any(|member| member.state == "ringing");
        let waiting = if call.role == "caller" && !call.answered {
            if has_ringing {
                "Đang đổ chuông…"
            } else {
                "Đang gọi nhóm…"
            }
        } else {
            "Đang chờ người khác…"
        };
        let text = if names.is_empty() {
            waiting.to_owned()
        } else {
            format!("{} người: Bạn, {}", names.len() + 1, names.join(", "))
        };
        if text != call.status_text {
            call.status_text = text.clone();
            self.call = Some(call);
            self.ui.status(&text);
        }
    }

    /// The server removed us from the room.
    pub fn group_ended(&mut self, why: &str) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if !call.group || call.state == "ended" {
            return;
        }
        let status = if call.state == "connected" { 50 } else { 0 };
        let text = if why == "kicked" {
            "Bạn đã bị mời ra khỏi cuộc gọi"
        } else {
            "Cuộc gọi nhóm đã kết thúc"
        };
        self._group_finish(status, Some(text), true);
    }

    fn _group_hangup(&mut self, why: &str) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if call.state == "ended" {
            return;
        }
        if call.state == "incoming" {
            self._group_reject(why);
            return;
        }
        if call.role == "caller" && !call.answered {
            self._group_cancel(why);
            return;
        }
        let status = if call.state == "connected" { 50 } else { 0 };
        self._group_finish(status, None, true);
    }

    fn _group_cancel(&mut self, why: &str) {
        let Some(call) = self.call.clone() else {
            return;
        };
        if call.host_call != 0 {
            self.emit(json!({
                "type": "sendSignal",
                "command": 12437,
                "data": {
                    "callId": call.call_id,
                    "data": json_cpp(&json!({
                        "callType": call.group_call_type,
                        "duration": 0,
                        "extraData": "",
                        "groupId": call.group_id,
                    })),
                    "hostCall": call.host_call,
                }
            }));
        }
        let text = if why == "no-answer" {
            "Không ai trả lời"
        } else {
            "Đã huỷ cuộc gọi nhóm"
        };
        self._group_finish(0, Some(text), false);
    }

    fn _schedule_group_finish(&mut self, status: i64, text: &str, signals: bool, delay_ms: u64) {
        if let Some(call) = self.call.as_mut() {
            call.state = "failed".to_owned();
            call.deferred_finish = Some(DeferredFinish {
                at: now_ms() + delay_ms,
                status,
                text: text.to_owned(),
                signals,
            });
        }
    }

    fn _group_finish(&mut self, status: i64, close_text: Option<&str>, signals: bool) {
        let Some(mut call) = self.call.clone() else {
            return;
        };
        if call.state == "ended" {
            return;
        }
        let connected = call.state == "connected";
        call.state = "ended".to_owned();
        let duration = if connected {
            now_ms().saturating_sub(call.started_at) / 1000
        } else {
            0
        };
        let stats = if connected {
            self.media.stats()
        } else {
            Value::Null
        };
        self.media.stop();
        if signals && connected {
            self.emit(json!({
                "type": "sendSignal",
                "command": 12438,
                "data": {
                    "callId": call.call_id,
                    "data": json_cpp(&json!({
                        "callType": call.group_call_type,
                        "duration": duration,
                        "extraData": "",
                        "groupId": call.group_id,
                        "status": 0,
                    })),
                    "hostCall": call.host_call,
                }
            }));
            self.emit(json!({
                "type": "sendSignal",
                "command": 12446,
                "data": {
                    "callId": call.call_id,
                    "callType": call.group_call_type,
                    "data": format!(
                        "{{\"Codec\":[\"opus/16000/1\",\"20\"],\"RxTotalPkt\":{},\"TxTotalPkt\":{},\"sysInfo\":\"native-engine\"}}",
                        stat_num(&stats, "rx"),
                        stat_num(&stats, "tx")
                    ),
                    "duration": duration,
                    "hostCall": call.host_call,
                    "joinTime": call.started_at,
                    "status": status,
                }
            }));
        }
        self.emit(json!({"type": "update", "command": "callState", "data": {"state": "free"}}));
        let close = close_text.map(str::to_owned).unwrap_or_else(|| {
            if connected {
                format!("Kết thúc — {duration} s")
            } else {
                "Đã kết thúc".to_owned()
            }
        });
        self.ui.close(&close);
        call.deferred_finish = None;
        self.call = None;
    }
}

fn cmd_str(command: Option<&Value>) -> Option<&str> {
    command.and_then(Value::as_str)
}

fn str_or(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// The callSetting server whose rtp/rtcp address is the InitZRTP winner.
fn server_for_best(servers: &[Value], best: Option<&Value>) -> Value {
    let fallback = servers.first().cloned().unwrap_or_else(|| json!({}));
    let Some(srv) = best.and_then(|value| value.get("server")) else {
        return fallback;
    };
    let Some(host) = srv
        .get("host")
        .and_then(Value::as_str)
        .filter(|host| !host.is_empty())
    else {
        return fallback;
    };
    let Some(port) = srv.get("port").and_then(Value::as_u64) else {
        return fallback;
    };
    let addr = format!("{host}:{port}");
    servers
        .iter()
        .find(|raw| {
            let rtp = str_or(raw, "rtpaddr").replace('|', ":");
            let rtcp = str_or(raw, "rtcpaddr").replace('|', ":");
            rtp == addr || rtcp == addr
        })
        .cloned()
        .unwrap_or(fallback)
}

fn first_nonempty(value: &Value, primary: &str, fallback: &str) -> String {
    let first = str_or(value, primary);
    if first.is_empty() {
        str_or(value, fallback)
    } else {
        first
    }
}

fn num(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(json_i64)
}

fn call_id_field(value: &Value) -> Option<i64> {
    value
        .get("callId")
        .or_else(|| value.get("id"))
        .and_then(json_i64)
}

/// JSON number or a digit string. `Number("1834567890")` in the Electron
/// engine; `Value::as_i64` alone is `None` for a string and the call id
/// collapses to 0.
fn json_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            if f.is_finite() && f.fract() == 0.0 && (i64::MIN as f64..=i64::MAX as f64).contains(&f)
            {
                Some(f as i64)
            } else {
                None
            }
        }),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else if let Ok(n) = s.parse::<i64>() {
                Some(n)
            } else {
                let f: f64 = s.parse().ok()?;
                if f.is_finite()
                    && f.fract() == 0.0
                    && (i64::MIN as f64..=i64::MAX as f64).contains(&f)
                {
                    Some(f as i64)
                } else {
                    None
                }
            }
        }
        _ => None,
    }
}

fn parse_maybe_json(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
        Some(other) => other.clone(),
        None => json!({}),
    }
}

/// JsonCpp writes object keys in order and a trailing newline.
fn json_cpp(value: &Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string(&sort_json(value)).unwrap_or_else(|_| "{}".into())
    )
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sort_json).collect()),
        Value::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                if let Some(item) = map.get(key) {
                    sorted.insert(key.clone(), sort_json(item));
                }
            }
            Value::Object(sorted)
        }
        other => other.clone(),
    }
}

fn merge_signal_body(data: &Value) -> Value {
    let mut base = parse_maybe_json(data.get("data"));
    if !base.is_object() {
        base = json!({});
    }
    let extra = parse_maybe_json(data.get("params"));
    if let (Some(obj), Some(more)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            obj.insert(key.clone(), value.clone());
        }
    }
    base
}

fn stat_num(stats: &Value, key: &str) -> u64 {
    stats.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Generates a call id that fits a signed 32-bit int.
pub fn new_call_id() -> i64 {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    1_000_000_000 + rng.gen_range(0..0x7fff_ffff - 1_000_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const CALL_ID: i64 = 1834567890;

    #[derive(Clone)]
    struct TestMedia {
        started: Rc<RefCell<Vec<Value>>>,
        events: Rc<RefCell<Vec<String>>>,
    }

    impl Media for TestMedia {
        fn local_candidates(&self, public_addr: Option<&str>) -> Option<Vec<Value>> {
            let pub_ip = public_addr.unwrap_or("").split('|').next().unwrap_or("");
            Some(vec![
                json!({"ip": "192.168.1.14", "port": 40000, "type": 0}),
                json!({"ip": pub_ip, "port": 40000, "type": 1}),
            ])
        }
        fn start(&mut self, role: &str, rtp_ser_ip: Option<&str>, opts: &Value) {
            self.started
                .borrow_mut()
                .push(json!({"role": role, "rtpSerIp": rtp_ser_ip, "opts": opts}));
        }
        fn set_mute(&mut self, on: bool) {
            self.events.borrow_mut().push(format!("mute:{on}"));
        }
        fn set_speaker(&mut self, off: bool) {
            self.events.borrow_mut().push(format!("speaker:{off}"));
        }
        fn set_mic_volume(&mut self, percent: u8) {
            self.events.borrow_mut().push(format!("vol:{percent}"));
        }
        fn request_peer_key_frame(&mut self, why: &str, src: Option<u32>) {
            self.events
                .borrow_mut()
                .push(format!("pli:{why}:{}", src.unwrap_or(0)));
        }
    }

    #[derive(Clone)]
    struct TestUi {
        incoming_calls: Rc<RefCell<usize>>,
        closed: Rc<RefCell<Vec<String>>>,
        notices: Rc<RefCell<Vec<String>>>,
        keys: Rc<RefCell<u32>>,
    }

    impl Ui for TestUi {
        fn incoming(&mut self, _info: &Value) {
            *self.incoming_calls.borrow_mut() += 1;
        }
        fn close(&mut self, text: &str) {
            self.closed.borrow_mut().push(text.to_owned());
        }
        fn notice(&mut self, text: &str) {
            self.notices.borrow_mut().push(text.to_owned());
        }
        fn request_key_frame(&mut self) {
            *self.keys.borrow_mut() += 1;
        }
    }

    struct Harness {
        engine: EngineCore,
        started: Rc<RefCell<Vec<Value>>>,
        incoming_calls: Rc<RefCell<usize>>,
        closed: Rc<RefCell<Vec<String>>>,
        notices: Rc<RefCell<Vec<String>>>,
        keys: Rc<RefCell<u32>>,
        events: Rc<RefCell<Vec<String>>>,
    }

    fn setup() -> Harness {
        let started = Rc::new(RefCell::new(Vec::new()));
        let incoming_calls = Rc::new(RefCell::new(0_usize));
        let closed = Rc::new(RefCell::new(Vec::new()));
        let notices = Rc::new(RefCell::new(Vec::new()));
        let keys = Rc::new(RefCell::new(0_u32));
        let events = Rc::new(RefCell::new(Vec::new()));
        let media = TestMedia {
            started: started.clone(),
            events: events.clone(),
        };
        let ui = TestUi {
            incoming_calls: incoming_calls.clone(),
            closed: closed.clone(),
            notices: notices.clone(),
            keys: keys.clone(),
        };
        let engine = EngineCore::new(Box::new(media), Box::new(ui));
        Harness {
            engine,
            started,
            incoming_calls,
            closed,
            notices,
            keys,
            events,
        }
    }

    fn ring(engine: &mut EngineCore, call_id: i64) {
        engine.on_zalo_frame(&json!({
            "type": "control",
            "data": {
                "act": "request",
                "data": {
                    "callId": call_id,
                    "uidN": "1234567890123456789",
                    "uidFrom": 123456789,
                    "uidTo": 987654321,
                    "session": "sess",
                    "rtpAddress": "1.2.3.4:5000",
                    "rtcpAddress": "1.2.3.4:5001",
                    "params": serde_json::to_string(&json!({
                        "Dname": "Peer",
                        "extendData": serde_json::to_string(&json!({"callType": 0, "serverAddr": []})).unwrap(),
                    })).unwrap(),
                }
            }
        }));
    }

    fn control(act: &str, data: Value) -> Value {
        json!({"type": "control", "data": {"act": act, "data": data}})
    }

    fn sent(frames: &[Value], command: i64) -> Vec<Value> {
        frames
            .iter()
            .filter(|f| {
                f.get("type").and_then(Value::as_str) == Some("sendSignal")
                    && f.get("command").and_then(Value::as_i64) == Some(command)
            })
            .cloned()
            .collect()
    }

    #[test]
    fn answered_on_another_device_closes_without_signals() {
        let mut h = setup();
        ring(&mut h.engine, CALL_ID);
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("incoming".to_owned())
        );
        assert_eq!(*h.incoming_calls.borrow(), 1);
        let before = h.engine.frames.len();

        h.engine
            .on_zalo_frame(&control("answer", json!({"callId": CALL_ID})));
        h.engine
            .on_zalo_frame(&control("answer_ack", json!({"callId": CALL_ID})));

        assert!(h.engine.call.is_none());
        let rest: Vec<Value> = h.engine.frames[before..].to_vec();
        assert_eq!(
            rest,
            vec![json!({"type": "update", "command": "callState", "data": {"state": "free"}})]
        );
        assert_eq!(sent(&h.engine.frames, 402).len(), 0);
        assert_eq!(sent(&h.engine.frames, 406).len(), 0);
        assert_eq!(
            h.engine
                .frames
                .iter()
                .filter(|f| f.get("command").and_then(Value::as_str) == Some("bubble"))
                .count(),
            0
        );
        assert_eq!(h.closed.borrow().len(), 1);

        h.engine
            .on_zalo_frame(&control("end_call", json!({"callId": CALL_ID})));
        assert_eq!(sent(&h.engine.frames, 406).len(), 0);

        ring(&mut h.engine, CALL_ID);
        ring(&mut h.engine, CALL_ID + 1);
        assert_eq!(*h.incoming_calls.borrow(), 2);
    }

    #[test]
    fn incoming_string_call_id_is_the_number_electron_parses() {
        let mut h = setup();
        h.engine.on_zalo_frame(&json!({
            "type": "control",
            "data": {
                "act": "request",
                "data": {
                    "callId": "1834567890",
                    "uidN": "1234567890123456789",
                    "uidFrom": "123456789",
                    "uidTo": "987654321",
                    "session": "sess",
                    "rtpAddress": "1.2.3.4:5000",
                    "rtcpAddress": "1.2.3.4:5001",
                    "params": serde_json::to_string(&json!({
                        "Dname": "Peer",
                        "extendData": serde_json::to_string(&json!({"callType": 0, "serverAddr": []})).unwrap(),
                    })).unwrap(),
                }
            }
        }));
        let c = h.engine.call.as_ref().expect("incoming");
        assert_eq!(c.call_id, CALL_ID);
        assert_eq!(c.peer_uid, 123_456_789);
        assert_eq!(c.local_uid, 987_654_321);
        let ring = sent(&h.engine.frames, 407);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring[0]["data"]["callId"], CALL_ID);

        h.engine
            .on_zalo_frame(&control("answer", json!({"callId": "1834567890"})));
        assert!(h.engine.call.is_none());
    }

    #[test]
    fn answer_for_another_call_id_leaves_ringing_alone() {
        let mut h = setup();
        ring(&mut h.engine, CALL_ID);
        h.engine
            .on_zalo_frame(&control("answer", json!({"callId": CALL_ID + 1})));
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("incoming".to_owned())
        );
        assert_eq!(h.closed.borrow().len(), 0);
    }

    #[test]
    fn answer_after_we_picked_up_changes_nothing() {
        let mut h = setup();
        ring(&mut h.engine, CALL_ID);
        h.engine.answer(false);
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("connected".to_owned())
        );
        h.engine
            .on_zalo_frame(&control("answer", json!({"callId": CALL_ID})));
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("connected".to_owned())
        );
        assert_eq!(h.closed.borrow().len(), 0);
        let free = h
            .engine
            .frames
            .iter()
            .filter(|f| {
                f.get("type").and_then(Value::as_str) == Some("update")
                    && f.get("command").and_then(Value::as_str) == Some("callState")
                    && f.get("data")
                        .and_then(|d| d.get("state"))
                        .and_then(Value::as_str)
                        == Some("free")
            })
            .count();
        assert_eq!(free, 0);
    }

    #[test]
    fn caller_side_callee_answer_connects_and_sends_408() {
        let mut h = setup();
        h.engine.call = Some(Call {
            role: "caller".to_owned(),
            state: "ringing".to_owned(),
            call_id: CALL_ID,
            peer_id: "1".to_owned(),
            call_type: 1,
            ..Call::default()
        });
        h.engine.on_zalo_frame(&control("answer", json!({"callId": CALL_ID, "params": serde_json::to_string(&json!({"status": 0})).unwrap()})));
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("connected".to_owned())
        );
        assert_eq!(sent(&h.engine.frames, 408).len(), 1);
    }

    #[test]
    fn caller_answer_matches_a_string_call_id() {
        let mut h = setup();
        h.engine.call = Some(Call {
            role: "caller".to_owned(),
            state: "ringing".to_owned(),
            call_id: CALL_ID,
            peer_id: "1".to_owned(),
            call_type: 1,
            ..Call::default()
        });
        let params = serde_json::to_string(&json!({"status": 0})).unwrap();
        h.engine.on_zalo_frame(&control(
            "answer",
            json!({"callId": "1834567891", "params": params}),
        ));
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("ringing".to_owned())
        );
        h.engine.on_zalo_frame(&control(
            "answer",
            json!({"callId": "1834567890", "params": serde_json::to_string(&json!({"status": 0})).unwrap()}),
        ));
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("connected".to_owned())
        );
    }

    #[test]
    fn caller_416_offers_p2p_and_answer_reaches_media() {
        let mut h = setup();
        h.engine.call = Some(Call {
            role: "caller".to_owned(),
            state: "calling".to_owned(),
            call_id: CALL_ID,
            peer_id: "1".to_owned(),
            call_type: 1,
            sess_id: "s".to_owned(),
            ..Call::default()
        });
        let c = h.engine.call.clone().unwrap();
        h.engine._send416(
            &c,
            &json!({"rtpaddr": "1.2.3.4:4200", "rtcpaddr": "1.2.3.4:4200"}),
            Some("5.6.7.8|40000"),
        );
        let frame = sent(&h.engine.frames, 416).pop().unwrap();
        let ext: Value =
            serde_json::from_str(frame["data"]["extendData"].as_str().unwrap()).unwrap();
        assert_eq!(ext["sP2P"], 1);
        let types: Vec<i64> = ext["p2p"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["type"].as_i64().unwrap())
            .collect();
        assert_eq!(types, vec![0, 1]);

        let cands = json!([{"ip": "192.168.1.36", "port": 65509, "type": 0}]);
        h.engine.on_zalo_frame(&control(
            "answer",
            json!({
                "callId": CALL_ID,
                "params": serde_json::to_string(&json!({
                    "status": 0,
                    "rtpSerIp": "1.2.3.4:4200",
                    "extendData": serde_json::to_string(&json!({"p2p": cands})).unwrap(),
                })).unwrap(),
            }),
        ));
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("connected".to_owned())
        );
        let started = h.started.borrow();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0]["role"], "caller");
        assert_eq!(started[0]["rtpSerIp"], "1.2.3.4:4200");
        assert_eq!(
            started[0]["opts"],
            json!({"p2pCandidates": [{"ip": "192.168.1.36", "port": 65509, "type": 0}]})
        );
    }

    #[test]
    fn caller_video_connect_repeats_camera_state() {
        let mut h = setup();
        h.engine.call = Some(Call {
            role: "caller".to_owned(),
            state: "ringing".to_owned(),
            call_id: CALL_ID,
            peer_id: "1".to_owned(),
            call_type: 3,
            ..Call::default()
        });
        h.engine._cam_mic(Some(true));
        h.engine.on_zalo_frame(&control("answer", json!({"callId": CALL_ID, "params": serde_json::to_string(&json!({"status": 0})).unwrap()})));
        let cams: Vec<i64> = sent(&h.engine.frames, 12006)
            .iter()
            .map(|f| f["data"]["isOnCam"].as_i64().unwrap())
            .collect();
        assert_eq!(cams, vec![1, 1]);
    }

    fn make_group(engine: &mut EngineCore) {
        engine.on_zalo_frame(&json!({
            "type": "request",
            "command": "makeCall",
            "data": {
                "type": 6,
                "groupInfo": {"id": "g1", "name": "Nhóm", "avatar": "", "maxUsers": 8},
                "partner": [{"id": "111", "name": "An"}]
            }
        }));
    }

    fn answer_group_request(engine: &mut EngineCore) {
        engine.on_zalo_frame(&json!({
            "type": "recvSignal",
            "command": 12433,
            "data": {
                "hostCall": 42,
                "params": serde_json::to_string(&json!({
                    "status": 0,
                    "callId": CALL_ID,
                    "groupId": 99,
                    "interval": 9,
                    "callSetting": {
                        "session": "sess-group",
                        "servers": [{"rtpaddr": "1.2.3.4:5000", "rtcpaddr": "1.2.3.4:5001"}]
                    }
                })).unwrap()
            }
        }));
    }

    fn ring_group(engine: &mut EngineCore) {
        engine.on_zalo_frame(&control(
            "group_request",
            json!({
                "id": CALL_ID,
                "hostCall": 77,
                "receiverId": 42,
                "groupId": 9,
                "groupName": "Nhóm",
                "callType": 1,
                "interval": 10,
                "callSetting": {
                    "session": "sess-in",
                    "servers": [{"rtpaddr": "1.2.3.4:5000", "rtcpaddr": "1.2.3.4:5001"}]
                },
                "partnerInfo": [{
                    "userId": 77,
                    "name": "Host",
                    "callState": 3,
                    "audioState": 0,
                    "videoState": 1
                }]
            }),
        ));
    }

    #[test]
    fn group_make_call_emits_12433() {
        let mut h = setup();
        make_group(&mut h.engine);
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("calling".to_owned())
        );
        assert_eq!(h.engine.call.as_ref().map(|c| c.group_call_type), Some(1));
        let frame = sent(&h.engine.frames, 12433).pop().expect("12433");
        let partners = frame["data"]["partners"].as_str().unwrap();
        assert!(partners.contains("111"));
        assert!(partners.ends_with('\n'));
        assert!(sent(&h.engine.frames, 401).is_empty());
    }

    #[test]
    fn group_request_answer_connects_and_joins() {
        let mut h = setup();
        make_group(&mut h.engine);
        answer_group_request(&mut h.engine);
        let call = h.engine.call.clone().expect("call");
        assert_eq!(call.state, "ringing");
        assert_eq!(call.started_at, 0);
        assert_eq!(call.call_id, CALL_ID);
        assert_eq!(call.local_uid, 42);
        assert!(call.camera_on);
        let broadcast = &sent(&h.engine.frames, 12098)[0];
        assert_eq!(broadcast["data"]["videoState"], 0);
        let frame = sent(&h.engine.frames, 12434).pop().expect("12434");
        assert_eq!(frame["data"]["calleeId"], "111");
        assert!(frame["data"].get("imei").is_none());
        assert!(frame["data"]["data"]
            .as_str()
            .unwrap()
            .contains("1.2.3.4:5000"));
        assert_eq!(sent(&h.engine.frames, 12098).len(), 1);
        let started = h.started.borrow();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0]["role"], "caller");
        drop(started);
        h.engine.group_roster(&[77]);
        let call = h.engine.call.clone().expect("still up");
        assert_eq!(call.state, "connected");
        assert!(call.started_at > 0);
        assert!(call.answered);
    }

    #[test]
    fn group_ring_status_ends_the_attempt() {
        let mut h = setup();
        make_group(&mut h.engine);
        answer_group_request(&mut h.engine);
        h.engine.on_zalo_frame(&json!({
            "type": "recvSignal",
            "command": 12434,
            "data": {"status": 2, "msg": null, "params": "{}"}
        }));
        let call = h.engine.call.clone().expect("stays open briefly");
        assert_eq!(call.state, "failed");
        assert_eq!(call.started_at, 0);
        assert!(call.status_text.contains("mã 2"));
    }

    #[test]
    fn group_ringing_times_out_without_an_answer() {
        let mut h = setup();
        make_group(&mut h.engine);
        answer_group_request(&mut h.engine);
        let created = h.engine.call.as_ref().unwrap().created_at;
        h.engine.tick_at(created + 59_999);
        assert_eq!(h.engine.call.as_ref().unwrap().state, "ringing");
        h.engine.tick_at(created + 60_000);
        assert!(h.engine.call.is_none());
        assert_eq!(sent(&h.engine.frames, 12437).len(), 1);
    }

    #[test]
    fn group_request_rings_with_12439() {
        let mut h = setup();
        ring_group(&mut h.engine);
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("incoming".to_owned())
        );
        assert_eq!(*h.incoming_calls.borrow(), 1);
        assert_eq!(sent(&h.engine.frames, 12439).len(), 1);
    }

    #[test]
    fn group_answer_sends_12436_and_broadcast() {
        let mut h = setup();
        ring_group(&mut h.engine);
        h.engine.answer(false);
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("connected".to_owned())
        );
        let frame = sent(&h.engine.frames, 12436).pop().expect("12436");
        assert_eq!(frame["data"]["status"], 0);
        assert!(h.engine.call.as_ref().unwrap().camera_on);
        let broadcast = &sent(&h.engine.frames, 12098)[0];
        assert_eq!(broadcast["data"]["videoState"], 0);
    }

    #[test]
    fn group_reject_sends_decline_without_end() {
        let mut h = setup();
        ring_group(&mut h.engine);
        h.engine.reject("user");
        assert!(h.engine.call.is_none());
        let frame = sent(&h.engine.frames, 12436).pop().expect("12436");
        assert_eq!(frame["data"]["status"], 3);
        assert!(sent(&h.engine.frames, 12438).is_empty());
    }

    #[test]
    fn last_group_member_left_ends_the_call() {
        let mut h = setup();
        make_group(&mut h.engine);
        answer_group_request(&mut h.engine);
        h.engine.group_roster(&[77]);
        assert!(h.engine.call.as_ref().unwrap().answered);
        h.engine.group_member_left(77, "left");
        assert!(h.engine.call.is_none());
        assert_eq!(sent(&h.engine.frames, 12438).len(), 1);
    }

    #[test]
    fn disabled_group_does_not_signal() {
        let mut h = setup();
        h.engine.group_enabled = false;
        make_group(&mut h.engine);
        assert!(h.engine.call.is_none());
        assert!(sent(&h.engine.frames, 12433).is_empty());
        assert!(h.engine.notice_close_text.contains("chưa được hỗ trợ"));
    }

    #[test]
    fn one_to_one_screen_share_emits_12064() {
        let mut h = setup();
        h.engine.call = Some(Call {
            role: "caller".to_owned(),
            state: "connected".to_owned(),
            call_id: CALL_ID,
            peer_id: "9".to_owned(),
            ..Call::default()
        });
        h.engine.screen_share(true);
        let frame = sent(&h.engine.frames, 12064).pop().expect("12064");
        assert_eq!(frame["data"]["status"], 1);
        assert_eq!(frame["data"]["uidTo"], "9");
    }

    #[test]
    fn group_share_answer_stores_the_id_and_sends_it_back() {
        let mut h = setup();
        h.engine.call = Some(Call {
            group: true,
            role: "caller".to_owned(),
            state: "connected".to_owned(),
            sharing: true,
            call_id: CALL_ID,
            local_uid: 42,
            host_call: 42,
            ..Call::default()
        });
        h.engine.on_zalo_frame(&json!({
            "type": "recvSignal",
            "command": 12044,
            "data": { "params": { "shareScreenId": 9001 } }
        }));
        assert_eq!(h.engine.call.as_ref().unwrap().share_screen_id, 9001);
        let frame = sent(&h.engine.frames, 12044).pop().expect("12044");
        assert_eq!(frame["data"]["status"], 1);
        assert!(frame["data"]["data"]
            .as_str()
            .unwrap_or("")
            .contains("9001"));
    }

    #[test]
    fn group_ping_follows_the_interval() {
        let mut h = setup();
        h.engine.call = Some(Call {
            group: true,
            role: "caller".to_owned(),
            state: "connected".to_owned(),
            call_id: CALL_ID,
            local_uid: 42,
            host_call: 42,
            answered: true,
            ping_ms: 1_000,
            last_ping_at: 0,
            ring_limit_ms: 0,
            ..Call::default()
        });
        h.engine.tick_at(5_000);
        assert_eq!(sent(&h.engine.frames, 12097).len(), 1);
        assert_eq!(h.engine.call.as_ref().unwrap().last_ping_at, 5_000);
    }

    fn connected_call() -> Call {
        Call {
            role: "caller".to_owned(),
            state: "connected".to_owned(),
            call_id: CALL_ID,
            peer_id: "9".to_owned(),
            ..Call::default()
        }
    }

    #[test]
    fn mute_on_a_connected_call_sends_12006() {
        let mut h = setup();
        h.engine.call = Some(connected_call());
        h.engine.mute(true);
        let frame = sent(&h.engine.frames, 12006).pop().expect("12006");
        assert_eq!(frame["data"]["isOnMic"], 1);
        assert_eq!(frame["data"]["isOnCam"], -1);
        assert!(h.engine.call.as_ref().unwrap().muted);
        assert_eq!(h.events.borrow().as_slice(), ["mute:true"]);
        h.engine.frames.clear();
        h.engine.mute(false);
        let frame = sent(&h.engine.frames, 12006).pop().expect("12006");
        assert_eq!(frame["data"]["isOnMic"], 0);
    }

    #[test]
    fn mute_before_connect_does_not_signal() {
        let mut h = setup();
        ring(&mut h.engine, CALL_ID);
        h.engine.frames.clear();
        h.engine.mute(true);
        assert!(sent(&h.engine.frames, 12006).is_empty());
        assert!(h.engine.call.as_ref().unwrap().muted);
    }

    #[test]
    fn group_mute_broadcasts_audio_state() {
        let mut h = setup();
        h.engine.call = Some(Call {
            group: true,
            state: "connected".to_owned(),
            call_id: CALL_ID,
            local_uid: 42,
            host_call: 42,
            ..Call::default()
        });
        h.engine.mute(true);
        let frame = sent(&h.engine.frames, 12098).pop().expect("12098");
        assert_eq!(frame["data"]["audioState"], 1);
    }

    #[test]
    fn speaker_and_volume_reach_the_backend() {
        let mut h = setup();
        h.engine.call = Some(connected_call());
        h.engine.speaker(true);
        h.engine.set_mic_volume(140);
        h.engine.set_mic_volume(-3);
        assert!(h.engine.call.as_ref().unwrap().speaker_off);
        assert_eq!(
            h.events.borrow().as_slice(),
            ["speaker:true", "vol:100", "vol:0"]
        );
    }

    #[test]
    fn setup_timeout_cancels_an_unanswered_call() {
        let mut h = setup();
        h.engine.call = Some(Call {
            role: "caller".to_owned(),
            state: "calling".to_owned(),
            call_id: CALL_ID,
            peer_id: "9".to_owned(),
            deadline_at: 15_000,
            ..Call::default()
        });
        h.engine.tick_at(15_000);
        assert!(h.engine.call.is_none());
        assert_eq!(sent(&h.engine.frames, 405).len(), 1);
        assert_eq!(sent(&h.engine.frames, 406).len(), 1);
        assert!(h
            .closed
            .borrow()
            .iter()
            .any(|t| t.contains("không liên lạc")));
    }

    #[test]
    fn ringing_timeout_says_no_answer() {
        let mut h = setup();
        h.engine.call = Some(Call {
            role: "caller".to_owned(),
            state: "ringing".to_owned(),
            call_id: CALL_ID,
            peer_id: "9".to_owned(),
            call_type: 3,
            deadline_at: 45_000,
            ..Call::default()
        });
        h.engine.tick_at(44_999);
        assert!(h.engine.call.is_some());
        h.engine.tick_at(45_000);
        assert!(h.engine.call.is_none());
        let frame = sent(&h.engine.frames, 405).pop().expect("405");
        assert_eq!(frame["data"]["callType"], 1);
        assert!(h
            .closed
            .borrow()
            .iter()
            .any(|t| t.contains("không trả lời")));
    }

    #[test]
    fn early_audio_rings_then_connects() {
        let mut h = setup();
        h.engine.call = Some(Call {
            role: "caller".to_owned(),
            state: "calling".to_owned(),
            call_id: CALL_ID,
            peer_id: "9".to_owned(),
            deadline_at: 99_000,
            ..Call::default()
        });
        h.engine.early_audio("first");
        assert_eq!(h.engine.call.as_ref().unwrap().state, "ringing");
        h.engine.early_audio("stream");
        assert_eq!(h.engine.call.as_ref().unwrap().state, "connected");
        assert_eq!(sent(&h.engine.frames, 408).len(), 1);
        assert_eq!(h.engine.call.as_ref().unwrap().deadline_at, 0);
    }

    #[test]
    fn audio_fault_is_shown_once_per_sentence() {
        let mut h = setup();
        h.engine.call = Some(connected_call());
        h.engine.audio_fault("mic");
        h.engine.audio_fault("mic");
        assert_eq!(h.notices.borrow().len(), 1);
        assert!(h.notices.borrow()[0].contains("micro"));
        h.engine.audio_fault("speaker");
        assert_eq!(h.notices.borrow().len(), 2);
        assert!(h.notices.borrow()[1].contains("micro và loa"));
        assert!(h.engine.call.is_some());
    }

    #[test]
    fn key_frame_requests_are_rate_limited() {
        let mut h = setup();
        h.engine.call = Some(connected_call());
        h.engine.request_key_frame("start");
        h.engine.request_key_frame("pli");
        assert_eq!(*h.keys.borrow(), 1);
        h.engine.call.as_mut().unwrap().key_asked_at = 1;
        h.engine.request_key_frame("pli");
        assert_eq!(*h.keys.borrow(), 2);
        h.engine.ask_peer_key("gap", Some(77));
        assert_eq!(h.events.borrow().as_slice(), ["pli:gap:77"]);
        h.engine.call.as_mut().unwrap().state = "calling".to_owned();
        h.engine.ask_peer_key("gap", None);
        assert_eq!(h.events.borrow().len(), 1);
    }

    #[test]
    fn stale_ended_call_can_ring_again() {
        let mut h = setup();
        h.engine.ended_calls.insert(CALL_ID, 1);
        ring(&mut h.engine, CALL_ID);
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("incoming".to_owned())
        );
    }

    #[test]
    fn ended_call_memory_stays_capped() {
        let mut h = setup();
        ring(&mut h.engine, CALL_ID);
        for id in 10_000..10_300 {
            h.engine
                .on_zalo_frame(&control("answer", json!({"callId": id})));
        }
        assert_eq!(h.engine.ended_calls.len(), 256);
        assert!(h.engine.ended_calls.contains_key(&10_299));
        assert!(!h.engine.ended_calls.contains_key(&10_000));
        assert_eq!(
            h.engine.call.as_ref().map(|c| c.state.clone()),
            Some("incoming".to_owned())
        );
    }

    #[test]
    fn outgoing_video_make_call_reports_camera_on_and_retries_after_connect() {
        let mut h = setup();
        h.engine.on_zalo_frame(&json!({
            "type": "request",
            "command": "makeCall",
            "data": {"type": 3, "partner": [{"id": "111", "name": "An"}]},
        }));
        let call = h.engine.call.as_ref().expect("outgoing call created");
        let cid = call.call_id;
        assert_eq!(call.call_type, 3);
        assert!(call.cam_reported);
        let cams_make: Vec<i64> = sent(&h.engine.frames, 12006)
            .iter()
            .map(|f| f["data"]["isOnCam"].as_i64().unwrap())
            .collect();
        assert_eq!(cams_make, vec![1]);

        h.engine.on_zalo_frame(&control(
            "answer",
            json!({"callId": cid, "params": serde_json::to_string(&json!({"status": 0})).unwrap()}),
        ));
        let cams_initial: Vec<i64> = sent(&h.engine.frames, 12006)
            .iter()
            .map(|f| f["data"]["isOnCam"].as_i64().unwrap())
            .collect();
        assert_eq!(cams_initial, vec![1, 1]);

        let retry_at = h.engine.call.as_ref().unwrap().cam_retry_at;
        assert!(retry_at > 0);
        h.engine.tick_at(retry_at - 1);
        assert_eq!(sent(&h.engine.frames, 12006).len(), 2);
        h.engine.tick_at(retry_at);
        let cams_after_retry: Vec<i64> = sent(&h.engine.frames, 12006)
            .iter()
            .map(|f| f["data"]["isOnCam"].as_i64().unwrap())
            .collect();
        assert_eq!(cams_after_retry, vec![1, 1, 1]);
        assert_eq!(h.engine.call.as_ref().unwrap().cam_retry_at, 0);
    }

    #[test]
    fn incoming_video_answer_reports_camera_on_and_respects_cam_start_off() {
        let ring_video = |engine: &mut EngineCore, call_id: i64| {
            engine.on_zalo_frame(&json!({
                "type": "control",
                "data": {
                    "act": "request",
                    "data": {
                        "callId": call_id,
                        "uidN": "1234567890123456789",
                        "uidFrom": 123456789,
                        "uidTo": 987654321,
                        "session": "sess",
                        "rtpAddress": "1.2.3.4:5000",
                        "rtcpAddress": "1.2.3.4:5001",
                        "params": serde_json::to_string(&json!({
                            "Dname": "Peer",
                            "extendData": serde_json::to_string(&json!({"callType": 1, "serverAddr": []})).unwrap(),
                        })).unwrap(),
                    }
                }
            }));
        };

        let mut h = setup();
        ring_video(&mut h.engine, CALL_ID);
        h.engine.answer(false);
        let cams: Vec<i64> = sent(&h.engine.frames, 12006)
            .iter()
            .map(|f| f["data"]["isOnCam"].as_i64().unwrap())
            .collect();
        assert_eq!(cams, vec![1]);
        let retry_at = h.engine.call.as_ref().unwrap().cam_retry_at;
        assert!(retry_at > 0);
        h.engine.tick_at(retry_at);
        let cams_after_retry: Vec<i64> = sent(&h.engine.frames, 12006)
            .iter()
            .map(|f| f["data"]["isOnCam"].as_i64().unwrap())
            .collect();
        assert_eq!(cams_after_retry, vec![1, 1]);

        let mut h_off = setup();
        ring_video(&mut h_off.engine, CALL_ID + 1);
        h_off.engine.answer(true);
        let cams_off: Vec<i64> = sent(&h_off.engine.frames, 12006)
            .iter()
            .map(|f| f["data"]["isOnCam"].as_i64().unwrap())
            .collect();
        assert_eq!(cams_off, vec![0]);
        assert_eq!(h_off.engine.call.as_ref().unwrap().cam_retry_at, 0);
        let started_at = h_off.engine.call.as_ref().unwrap().started_at;
        h_off.engine.tick_at(started_at.saturating_add(2_500));
        assert_eq!(sent(&h_off.engine.frames, 12006).len(), 1);
    }

    #[test]
    fn callee_reject_finishes_with_status_160() {
        let mut h = setup();
        h.engine.on_zalo_frame(&json!({
            "type": "request",
            "command": "makeCall",
            "data": {"type": 0, "partner": [{"id": "111", "name": "Alice"}]},
        }));
        let cid = h.engine.call.as_ref().expect("call").call_id;
        h.engine.on_zalo_frame(&control(
            "answer",
            json!({"callId": cid, "params": serde_json::to_string(&json!({"status": 3})).unwrap()}),
        ));
        let end_frame = sent(&h.engine.frames, 406).pop().expect("406 finish");
        assert_eq!(end_frame["data"]["status"], 160);
    }

    #[test]
    fn turning_camera_on_in_voice_call_emits_audio_to_video_upgrade_12056() {
        let mut h = setup();
        ring(&mut h.engine, CALL_ID);
        h.engine.answer(false);
        assert_ne!(h.engine.call.as_ref().unwrap().call_type, 3);
        assert!(sent(&h.engine.frames, 12056).is_empty());

        h.engine._cam_mic(Some(true));
        let upgrades = sent(&h.engine.frames, 12056);
        assert_eq!(upgrades.len(), 1);
        assert_eq!(upgrades[0]["data"]["status"], 1);
        assert_eq!(h.engine.call.as_ref().unwrap().call_type, 3);

        // Subsequent toggle off/on does not re-emit 12056 once upgraded.
        h.engine._cam_mic(Some(false));
        h.engine._cam_mic(Some(true));
        assert_eq!(sent(&h.engine.frames, 12056).len(), 1);
    }
}
