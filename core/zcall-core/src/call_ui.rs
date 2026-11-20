//! Call-window protocol from `zcall-native/native-engine/ui-client.js`.
//!
//! The engine writes one JSON object per line. The window writes one action
//! per line. This type keeps that state and does not open a socket.

use base64::Engine;
use serde_json::{json, Map, Value};

/// One click or frame from the call window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAction {
    /// The callee picked up. `cam_start_off` starts a video call with the camera off.
    Accept {
        /// Start with the camera off.
        cam_start_off: bool,
    },
    /// The callee declined, or hung up before answering.
    Reject {
        /// Reason string from the window.
        why: String,
    },
    /// Local hangup of a call that is already up.
    Hangup,
    /// Microphone mute.
    Mute {
        /// `true` mutes.
        on: bool,
    },
    /// Speaker mute.
    Speaker {
        /// `true` silences playout.
        off: bool,
    },
    /// Local camera.
    Camera {
        /// `true` turns the camera on.
        on: bool,
    },
    /// Screen share replaces the camera.
    Screen {
        /// `true` starts sharing.
        on: bool,
    },
    /// Microphone source volume from the window. The engine clamps it to 0..=100.
    MicVolume {
        /// Raw percent.
        percent: i64,
    },
    /// The decoder wants a key frame from `src` (omitted in a 1-1 call).
    NeedKey {
        /// Short reason, at most 40 characters.
        why: String,
        /// Group member SSRC, when present.
        src: Option<u32>,
    },
    /// PulseAudio source or sink. An empty id means the system default.
    Device {
        /// `mic` or `speaker`.
        kind: String,
        /// Device name.
        id: String,
    },
    /// A group tile was drawn this many pixels wide.
    Tile {
        /// Member SSRC.
        src: u32,
        /// Tile width in pixels.
        width: u32,
    },
    /// An already-encoded H.264 access unit from the window.
    VideoFrame {
        /// This access unit is a key frame.
        key: bool,
        /// Annex-B bytes.
        data: Vec<u8>,
        /// The frame is the screen share rather than the camera.
        screen: bool,
    },
    /// A diagnostic line from the window.
    Log {
        /// Text, truncated to 300 characters.
        text: String,
    },
}

/// Outbound call-window state and the parser for inbound actions.
#[derive(Clone, Debug, Default)]
pub struct CallUiModel {
    state: Option<Map<String, Value>>,
    /// JSON lines waiting to be written. Each line has no trailing newline.
    pub outbound: Vec<String>,
    incoming: bool,
}

impl CallUiModel {
    /// Builds an empty model. Nothing is sent until the engine opens a call.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the window is waiting for accept or reject.
    pub fn is_incoming(&self) -> bool {
        self.incoming
    }

    /// Opens an outgoing call.
    pub fn open(&mut self, info: &Value) {
        self.incoming = false;
        self.push_state(base_state("outgoing", info));
    }

    /// Shows an incoming call.
    pub fn incoming(&mut self, info: &Value) {
        self.incoming = true;
        let mut state = base_state("incoming", info);
        state.insert(
            "noAnswer".to_owned(),
            json!(info
                .get("noAnswer")
                .and_then(Value::as_bool)
                .unwrap_or(false)),
        );
        state.insert(
            "inviter".to_owned(),
            info.get("inviter").cloned().unwrap_or(json!("")),
        );
        self.push_state(state);
    }

    /// Updates the status line. A non-zero `since` marks the call connected.
    pub fn status(&mut self, text: &str, since: u64) {
        let Some(mut state) = self.state.clone() else {
            return;
        };
        let phase = state
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let next = if since > 0 {
            "connected"
        } else if phase == "incoming" {
            "connecting"
        } else {
            phase.as_str()
        };
        state.insert("phase".to_owned(), json!(next));
        state.insert("text".to_owned(), json!(text));
        state.insert("since".to_owned(), json!(since));
        self.state = Some(state);
        self.emit_state();
    }

    /// Keeps a device problem on screen after the status line is hidden.
    pub fn notice(&mut self, text: &str) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        state.insert("notice".to_owned(), json!(text));
        self.emit_state();
    }

    /// Peer camera, mute, and screen-share flags.
    pub fn peer(&mut self, peer: &Value) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        state.insert(
            "peerCamOff".to_owned(),
            json!(peer.get("camOff").and_then(Value::as_bool).unwrap_or(false)),
        );
        state.insert(
            "peerMuted".to_owned(),
            json!(peer.get("muted").and_then(Value::as_bool).unwrap_or(false)),
        );
        state.insert(
            "peerSharing".to_owned(),
            json!(peer
                .get("sharing")
                .and_then(Value::as_bool)
                .unwrap_or(false)),
        );
        self.emit_state();
    }

    /// Replaces the peer name after an alias arrives.
    pub fn rename(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        state.insert("name".to_owned(), json!(name));
        self.emit_state();
    }

    /// Replaces the group member tiles.
    pub fn members(&mut self, list: &Value) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        state.insert("members".to_owned(), list.clone());
        self.emit_state();
    }

    /// Asks the window to encode the next camera frame as a key frame.
    pub fn request_key_frame(&mut self) {
        self.send(&json!({"type": "keyframe"}));
    }

    /// Group: `src` started or stopped talking.
    pub fn speaking(&mut self, src: u32, on: bool) {
        self.send(&json!({"type": "speaking", "src": src, "on": on}));
    }

    /// Reports the microphone source volume, 0..=100.
    pub fn mic_volume(&mut self, percent: u8) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        state.insert("micVolume".to_owned(), json!(percent));
        self.emit_state();
    }

    /// Closes the window.
    pub fn close(&mut self, text: &str) {
        self.incoming = false;
        self.state = None;
        self.send(&json!({"type": "close", "text": text}));
    }

    /// Parses one inbound line. A line that is not JSON is ignored.
    pub fn push_line(&mut self, line: &str) -> Option<UiAction> {
        let msg: Value = serde_json::from_str(line.trim()).ok()?;
        self.on_action(&msg)
    }

    fn on_action(&mut self, msg: &Value) -> Option<UiAction> {
        match msg.get("action").and_then(Value::as_str)? {
            "accept" => {
                if !self.incoming {
                    return None;
                }
                self.incoming = false;
                Some(UiAction::Accept {
                    cam_start_off: msg
                        .get("camStartOff")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
            }
            "reject" => {
                if !self.incoming {
                    return None;
                }
                self.incoming = false;
                Some(UiAction::Reject {
                    why: msg
                        .get("why")
                        .and_then(Value::as_str)
                        .unwrap_or("reject")
                        .to_owned(),
                })
            }
            "hangup" => {
                if self.incoming {
                    self.incoming = false;
                    return Some(UiAction::Reject {
                        why: "reject".to_owned(),
                    });
                }
                Some(UiAction::Hangup)
            }
            "mute" => {
                let on = msg.get("on").and_then(Value::as_bool).unwrap_or(false);
                if let Some(state) = self.state.as_mut() {
                    state.insert("muted".to_owned(), json!(on));
                }
                Some(UiAction::Mute { on })
            }
            "speaker" => {
                let off = msg.get("on").and_then(Value::as_bool).unwrap_or(false);
                if let Some(state) = self.state.as_mut() {
                    state.insert("speakerOff".to_owned(), json!(off));
                }
                Some(UiAction::Speaker { off })
            }
            "camera" => Some(UiAction::Camera {
                on: msg.get("on").and_then(Value::as_bool).unwrap_or(false),
            }),
            "screen" => Some(UiAction::Screen {
                on: msg.get("on").and_then(Value::as_bool).unwrap_or(false),
            }),
            "micVolume" => {
                let value = msg.get("value").and_then(Value::as_i64)?;
                Some(UiAction::MicVolume { percent: value })
            }
            "needkey" => {
                let why = msg.get("why").and_then(Value::as_str).unwrap_or("decoder");
                let why: String = why.chars().take(40).collect();
                let src = msg
                    .get("src")
                    .and_then(Value::as_u64)
                    .map(|n| n as u32)
                    .filter(|n| *n > 0);
                Some(UiAction::NeedKey { why, src })
            }
            "device" => {
                let kind = msg.get("kind").and_then(Value::as_str)?;
                if kind != "mic" && kind != "speaker" {
                    return None;
                }
                let id = msg
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                Some(UiAction::Device {
                    kind: kind.to_owned(),
                    id,
                })
            }
            "tile" => Some(UiAction::Tile {
                src: msg.get("src").and_then(Value::as_u64).unwrap_or(0) as u32,
                width: msg.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
            }),
            "videoFrame" => {
                let data = msg.get("data").and_then(Value::as_str)?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap_or_default();
                Some(UiAction::VideoFrame {
                    key: msg.get("key").and_then(Value::as_bool).unwrap_or(false),
                    data: bytes,
                    screen: msg.get("screen").and_then(Value::as_bool).unwrap_or(false),
                })
            }
            "log" => {
                let text = msg.get("text").and_then(Value::as_str).unwrap_or("");
                let text: String = text.chars().take(300).collect();
                Some(UiAction::Log { text })
            }
            _ => None,
        }
    }

    fn push_state(&mut self, state: Map<String, Value>) {
        self.state = Some(state);
        self.emit_state();
    }

    fn emit_state(&mut self) {
        let Some(state) = self.state.clone() else {
            return;
        };
        let mut msg = state;
        msg.insert("type".to_owned(), json!("state"));
        self.send(&Value::Object(msg));
    }

    fn send(&mut self, msg: &Value) {
        if let Ok(line) = serde_json::to_string(msg) {
            self.outbound.push(line);
        }
    }
}

fn base_state(phase: &str, info: &Value) -> Map<String, Value> {
    let mut state = Map::new();
    state.insert("phase".to_owned(), json!(phase));
    state.insert(
        "name".to_owned(),
        info.get("title").cloned().unwrap_or(json!("")),
    );
    state.insert(
        "avatar".to_owned(),
        info.get("avatar").cloned().unwrap_or(json!("")),
    );
    state.insert(
        "text".to_owned(),
        info.get("text").cloned().unwrap_or(json!("")),
    );
    state.insert("since".to_owned(), json!(0));
    state.insert(
        "video".to_owned(),
        json!(info.get("video").and_then(Value::as_bool).unwrap_or(false)),
    );
    state.insert(
        "group".to_owned(),
        json!(info.get("group").and_then(Value::as_bool).unwrap_or(false)),
    );
    state.insert(
        "selfAvatar".to_owned(),
        info.get("selfAvatar").cloned().unwrap_or(json!("")),
    );
    state.insert("muted".to_owned(), json!(false));
    state.insert("speakerOff".to_owned(), json!(false));
    if let Some(members) = info.get("members") {
        state.insert("members".to_owned(), members.clone());
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_sends_an_outgoing_state() {
        let mut ui = CallUiModel::new();
        ui.open(&json!({"title": "An", "text": "Đang gọi…", "video": false}));
        let line = ui.outbound.pop().expect("state");
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], "state");
        assert_eq!(value["phase"], "outgoing");
        assert_eq!(value["name"], "An");
        assert_eq!(value["text"], "Đang gọi…");
    }

    #[test]
    fn status_with_a_clock_marks_the_call_connected() {
        let mut ui = CallUiModel::new();
        ui.incoming(&json!({"title": "An", "text": "Cuộc gọi thoại đến"}));
        ui.outbound.clear();
        ui.status("Đã kết nối", 1_700_000_000_000);
        let value: Value = serde_json::from_str(ui.outbound.last().unwrap()).unwrap();
        assert_eq!(value["phase"], "connected");
        assert_eq!(value["since"], 1_700_000_000_000_u64);
    }

    #[test]
    fn hangup_of_an_incoming_call_rejects() {
        let mut ui = CallUiModel::new();
        ui.incoming(&json!({"title": "An", "text": "đến"}));
        let action = ui.push_line(r#"{"action":"hangup"}"#);
        assert_eq!(
            action,
            Some(UiAction::Reject {
                why: "reject".into()
            })
        );
        assert!(!ui.is_incoming());
    }

    #[test]
    fn mute_and_needkey_parse() {
        let mut ui = CallUiModel::new();
        ui.open(&json!({"title": "An", "text": "Đang gọi…"}));
        assert_eq!(
            ui.push_line(r#"{"action":"mute","on":true}"#),
            Some(UiAction::Mute { on: true })
        );
        assert_eq!(
            ui.push_line(r#"{"action":"needkey","why":"decoder","src":77}"#),
            Some(UiAction::NeedKey {
                why: "decoder".into(),
                src: Some(77),
            })
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode([0, 0, 0, 1, 9]);
        let line = format!(r#"{{"action":"videoFrame","key":true,"data":"{encoded}"}}"#);
        match ui.push_line(&line) {
            Some(UiAction::VideoFrame { key, data, screen }) => {
                assert!(key);
                assert!(!screen);
                assert_eq!(data, vec![0, 0, 0, 1, 9]);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(ui.push_line("not json").is_none());
    }

    #[test]
    fn close_clears_the_state() {
        let mut ui = CallUiModel::new();
        ui.open(&json!({"title": "An", "text": "Đang gọi…"}));
        ui.outbound.clear();
        ui.close("Đã kết thúc");
        let value: Value = serde_json::from_str(&ui.outbound[0]).unwrap();
        assert_eq!(value["type"], "close");
        assert_eq!(value["text"], "Đã kết thúc");
        ui.status("sau khi đóng", 0);
        assert_eq!(ui.outbound.len(), 1);
    }
}
