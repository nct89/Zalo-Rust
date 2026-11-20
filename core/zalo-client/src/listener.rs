//! Realtime WebSocket listener for the `zpw_ws` endpoints.
//!
//! Frame layout: byte 0 is the version, bytes 1–2 the command (u16 little
//! endian), byte 3 the sub-command, and the rest a UTF-8 JSON body.
//!
//! After the socket opens the server sends `cmd 1 / sub 1` with `{"key": …}`,
//! the base64 AES-GCM key for later event bodies. Event bodies look like
//! `{"data": "...", "encrypt": n}`:
//!
//! | `encrypt` | body |
//! |---|---|
//! | 0 | plain JSON (string or object) |
//! | 1 | base64, zlib/gzip |
//! | 2 | base64, AES-GCM (16-byte IV, 16-byte AAD), then zlib/gzip |
//! | 3 | base64, AES-GCM, no compression |
//!
//! The client keeps the connection alive with `cmd 2 / sub 1` pings and
//! reconnects with backoff. A `cmd 3000` frame or close code 3000 means another
//! client took over the session; the listener stops instead of fighting it.

use aes_gcm::aead::consts::U16;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::aes::{Aes128, Aes256};
use aes_gcm::{AesGcm, Nonce};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use zalo_protocol::{api_type, base64_decode, client_version};

/// Endpoint used when neither getLoginInfo nor getServerInfo lists one.
pub const DEFAULT_WS_ENDPOINT: &str = "wss://ws1-msg.chat.zalo.me";

/// Ping period when the server settings do not give one.
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(30);

/// Command: cipher key handshake (`sub 1`).
pub const CMD_HANDSHAKE: u16 = 1;
/// Command: keep-alive ping (`sub 1`).
pub const CMD_PING: u16 = 2;
/// Command: direct messages (`sub 0`).
pub const CMD_USER_MESSAGES: u16 = 501;
/// Command: group messages (`sub 0`).
pub const CMD_GROUP_MESSAGES: u16 = 521;
/// Command: older direct messages after a reconnect (`510_0` / `510_1`).
pub const CMD_USER_CATCH_UP: u16 = 510;
/// Command: older group messages after a reconnect (`511_0` / `511_1`).
pub const CMD_GROUP_CATCH_UP: u16 = 511;
/// Extra offline message queues the server lists in `qCmds` (`513_0`…).
pub const CMD_QUEUE_513: u16 = 513;
/// Extra offline message queue `514_0`.
pub const CMD_QUEUE_514: u16 = 514;
/// Extra offline message queue `515_0`.
pub const CMD_QUEUE_515: u16 = 515;
/// Server says these message ids exist (`HAS_MSG`). The client answers with [`CMD_GET_MSG`].
pub const CMD_HAS_MSG: u16 = 533;
/// Message bodies the server pushes because they were missed (`PUSH_MISS_MSG`).
pub const CMD_PUSH_MISS: u16 = 534;
/// Fetch the bodies announced by [`CMD_HAS_MSG`] (`GET_MSG`).
pub const CMD_GET_MSG: u16 = 570;
/// Offline preload queue (`PRELOAD`), pulled when the auth payload lists it.
pub const CMD_PRELOAD: u16 = 571;
/// Offline message queue `710_0` (direct, id-zone 3).
pub const CMD_QUEUE_710: u16 = 710;
/// Offline message queue `711_0` (group, id-zone 3).
pub const CMD_QUEUE_711: u16 = 711;
/// Offline message queue `713_0`.
pub const CMD_QUEUE_713: u16 = 713;
/// Offline message queue `715_0`.
pub const CMD_QUEUE_715: u16 = 715;
/// How many ids one `GET_MSG` carries (`socket.max_msg_get`, default 20).
const GET_MSG_BATCH: usize = 20;
/// Command: typing indicators (`sub 0`).
pub const CMD_TYPING: u16 = 602;
/// Command: another client replaced this connection.
pub const CMD_DUPLICATE: u16 = 3000;
/// Delivered/seen status of our direct messages.
pub const CMD_USER_STATUS: u16 = 502;
/// Delivered/seen status of our group messages.
pub const CMD_GROUP_STATUS: u16 = 522;
/// Direct conversations read on another device.
pub const CMD_USER_CLEAR_UNREAD: u16 = 504;
/// Group conversations read on another device.
pub const CMD_GROUP_CLEAR_UNREAD: u16 = 524;
/// Live control events (`CTRL_PUSH_CTRL`). Voip calls use `act_type: "voip"`.
pub const CMD_PUSH_CTRL: u16 = 601;
/// Catch-up missed control events (`CTRL_PULL_CTRL`), same `controls` list as 601.
pub const CMD_PULL_CTRL: u16 = 603;
/// Catch-up missed reactions on a 1-1 thread.
pub const CMD_PULL_REACT: u16 = 610;
/// Catch-up missed reactions in a group.
pub const CMD_PULL_REACT_GROUP: u16 = 611;
/// A reaction was added (`CTRL_PUSH_REACT`).
pub const CMD_PUSH_REACT: u16 = 612;
/// A reaction was cleared (`CTRL_PUSH_CLEAR_REACT`).
pub const CMD_CLEAR_REACT: u16 = 613;

const MAX_BACKOFF: Duration = Duration::from_secs(60);
const CLOSE_DUPLICATE: u16 = 3000;

/// The listener could not decode a frame or keep the socket open.
#[derive(Debug, Error)]
pub enum ListenerError {
    /// The binary frame is shorter than the 4-byte header.
    #[error("frame is shorter than its header")]
    ShortFrame,
    /// A frame body or decoded event is not valid JSON.
    #[error("event JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    /// Base64 in an event body is invalid.
    #[error("event base64 is invalid: {0}")]
    Base64(#[from] zalo_protocol::CryptoError),
    /// AES-GCM rejected the body or the key length.
    #[error("event decryption failed")]
    Decrypt,
    /// An encrypted event arrived before the cipher key.
    #[error("encrypted event arrived before the cipher key")]
    NoCipherKey,
    /// zlib/gzip inflate failed.
    #[error("event inflate failed: {0}")]
    Inflate(#[from] std::io::Error),
    /// The event body has an unexpected shape.
    #[error("unexpected event: {0}")]
    Unexpected(String),
    /// WebSocket transport error.
    #[error("websocket: {0}")]
    Socket(#[from] Box<tokio_tungstenite::tungstenite::Error>),
    /// The endpoint URL is not valid.
    #[error("websocket URL is invalid: {0}")]
    Url(String),
    /// The server closed the socket.
    #[error("socket closed ({0})")]
    Closed(String),
    /// Repeated disconnects before handshake readiness; caller should refresh
    /// `getLoginInfo` cookies and WebSocket endpoints before reconnecting.
    #[error("session refresh required")]
    RefreshNeeded,
}

impl From<tokio_tungstenite::tungstenite::Error> for ListenerError {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Socket(Box::new(err))
    }
}

/// One binary WebSocket frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Protocol version. Zalo uses 1.
    pub version: u8,
    /// Command id.
    pub cmd: u16,
    /// Sub-command id.
    pub sub_cmd: u8,
    /// UTF-8 JSON body.
    pub body: Vec<u8>,
}

impl Frame {
    /// Builds a version-1 frame with a JSON body.
    pub fn json(cmd: u16, sub_cmd: u8, body: &Value) -> Self {
        Self {
            version: 1,
            cmd,
            sub_cmd,
            body: body.to_string().into_bytes(),
        }
    }

    /// The keep-alive ping frame.
    pub fn ping(event_id: i64) -> Self {
        Self::json(CMD_PING, 1, &serde_json::json!({ "eventId": event_id }))
    }

    /// Serialises the header and body.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.body.len());
        out.push(self.version);
        out.extend_from_slice(&self.cmd.to_le_bytes());
        out.push(self.sub_cmd);
        out.extend_from_slice(&self.body);
        out
    }

    /// Parses a binary frame.
    ///
    /// # Errors
    ///
    /// Returns [`ListenerError::ShortFrame`] for fewer than 4 bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, ListenerError> {
        if bytes.len() < 4 {
            return Err(ListenerError::ShortFrame);
        }
        Ok(Self {
            version: bytes[0],
            cmd: u16::from_le_bytes([bytes[1], bytes[2]]),
            sub_cmd: bytes[3],
            body: bytes[4..].to_vec(),
        })
    }

    /// The body parsed as JSON. An empty body is `null`.
    ///
    /// # Errors
    ///
    /// Returns [`ListenerError::Json`] when the body is not JSON.
    pub fn body_json(&self) -> Result<Value, ListenerError> {
        if self.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&self.body)?)
    }
}

/// Something the listener saw.
#[derive(Clone, Debug, PartialEq)]
pub enum ListenerEvent {
    /// The socket is open; waiting for the cipher key.
    Connected,
    /// The cipher key arrived; events can be decoded.
    Ready,
    /// New direct (`group == false`) or group messages, raw as Zalo sends them.
    Messages {
        /// `true` for `cmd 521`.
        group: bool,
        /// Message objects (`msgId`, `cliMsgId`, `uidFrom`, `idTo`, `content`, …).
        items: Vec<Value>,
        /// `true` for messages fetched to fill a gap after a reconnect
        /// (`cmd 510/511`). They are not new, so the app should not notify.
        history: bool,
        /// Read/seen marks that came in the same batch.
        status: crate::status::StatusUpdate,
    },
    /// Read on another device (`clearUnreads`) or seen by the other side,
    /// from `502/522/504/524` pushes.
    Status(crate::status::StatusUpdate),
    /// Someone is typing.
    Typing(Vec<Typing>),
    /// Any other decoded event (reactions 612, control 601, typing 602, seen 502/522…).
    Event {
        /// Command id.
        cmd: u16,
        /// Sub-command id.
        sub_cmd: u8,
        /// Decoded body.
        data: Value,
    },
    /// The socket dropped; the listener will reconnect.
    Disconnected(String),
    /// Another client took over this session; the listener stopped.
    Replaced,
}

/// Decoder state for one connection.
#[derive(Clone, Debug, Default)]
pub struct EventDecoder {
    cipher_key: Option<String>,
    /// `"cmd_sub"` → last `lastActionId` the server acknowledged.
    cursors: HashMap<String, String>,
    cursors_dirty: bool,
    /// Queues this connection asked the server to drain.
    pulling: HashSet<(u16, u8)>,
    /// `lastActionId` for queues Electron never persists (`518`, `519`).
    /// Stops a repeated id from paging forever, and stays out of [`Self::cursors`].
    volatile: HashMap<String, String>,
    /// Frames to write after [`Self::handle`] (`GET_MSG`, the next offline page).
    outbound: Vec<Frame>,
    req_seq: u64,
}

impl EventDecoder {
    /// Empty state (no cipher key yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Resume offline queues from cursors saved by a previous connection.
    pub fn with_cursors(cursors: HashMap<String, String>) -> Self {
        Self {
            cursors,
            ..Self::default()
        }
    }

    /// Whether the handshake key has arrived.
    pub fn is_ready(&self) -> bool {
        self.cipher_key.is_some()
    }

    /// Frames produced by the last [`Self::handle`] (offline pulls, `GET_MSG`).
    pub fn take_outbound(&mut self) -> Vec<Frame> {
        std::mem::take(&mut self.outbound)
    }

    /// Saved queue cursors. [`Self::cursors_dirty`] is set when one changed.
    pub fn cursors(&self) -> &HashMap<String, String> {
        &self.cursors
    }

    /// `true` after a `lastActionId` advanced.
    pub fn cursors_dirty(&self) -> bool {
        self.cursors_dirty
    }

    /// Clears [`Self::cursors_dirty`] after the caller has stored the map.
    pub fn clear_cursors_dirty(&mut self) {
        self.cursors_dirty = false;
    }

    /// Turns one frame into an event. `Ok(None)` is a frame with nothing to report
    /// (ping replies, empty batches, and `HAS_MSG`, which only queues a `GET_MSG`).
    ///
    /// # Errors
    ///
    /// Returns [`ListenerError`] when the body cannot be parsed or decrypted.
    pub fn handle(&mut self, frame: &Frame) -> Result<Option<ListenerEvent>, ListenerError> {
        if frame.cmd == CMD_HANDSHAKE && frame.sub_cmd == 1 {
            return self.on_auth(frame);
        }
        match (frame.cmd, frame.sub_cmd) {
            (CMD_PING, _) => Ok(None),
            (CMD_DUPLICATE, _) => Ok(Some(ListenerEvent::Replaced)),
            (CMD_HAS_MSG, _) => {
                let data = self.decode_body(frame)?;
                self.enqueue_get_msg(&data);
                Ok(None)
            }
            (CMD_USER_MESSAGES | CMD_GROUP_MESSAGES, 0) => self.live_messages(frame),
            (CMD_TYPING, 0) => {
                let data = self.decode_body(frame)?;
                let list = typing_from(&data);
                Ok((!list.is_empty()).then_some(ListenerEvent::Typing(list)))
            }
            (
                CMD_USER_STATUS | CMD_GROUP_STATUS | CMD_USER_CLEAR_UNREAD | CMD_GROUP_CLEAR_UNREAD,
                _,
            ) => {
                let data = self.decode_body(frame)?;
                self.note_page(frame.cmd, frame.sub_cmd, &data);
                let status = crate::status::StatusUpdate::from_event(&data);
                Ok(Some(if status.is_empty() {
                    ListenerEvent::Event {
                        cmd: frame.cmd,
                        sub_cmd: frame.sub_cmd,
                        data,
                    }
                } else {
                    ListenerEvent::Status(status)
                }))
            }
            (cmd, sub_cmd) => {
                let data = self.decode_body(frame)?;
                self.note_page(cmd, sub_cmd, &data);
                // An empty offline page (pagination only, or a `GET_MSG` ack) stays
                // quiet. Surfacing it as `Event` would look like a live control.
                if is_message_cmd(cmd) {
                    return Ok(queued_messages(cmd, &data));
                }
                Ok(Some(ListenerEvent::Event { cmd, sub_cmd, data }))
            }
        }
    }

    fn on_auth(&mut self, frame: &Frame) -> Result<Option<ListenerEvent>, ListenerError> {
        let body = frame.body_json()?;
        let Some(key) = body
            .get("key")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        self.cipher_key = Some(key.to_owned());
        let error = body.get("error_code").and_then(Value::as_i64).unwrap_or(0);
        if error == 0 && body.get("data").is_some() {
            if let Ok(plain) = decode_event_data(&body, Some(key)) {
                let mut queues = Vec::new();
                push_queue_list(&mut queues, &plain);
                if let Some(inner) = plain.get("data") {
                    push_queue_list(&mut queues, inner);
                }
                for (cmd, sub) in queues {
                    self.pulling.insert((cmd, sub));
                    let last = self.cursor_of(cmd, sub);
                    let req = self.alloc_req();
                    self.outbound
                        .push(offline_pull_frame(cmd, sub, true, &last, &req));
                }
            }
        }
        Ok(Some(ListenerEvent::Ready))
    }

    fn live_messages(&mut self, frame: &Frame) -> Result<Option<ListenerEvent>, ListenerError> {
        let data = self.decode_body(frame)?;
        let group = frame.cmd == CMD_GROUP_MESSAGES;
        let items = message_items(&data, group);
        let status = crate::status::StatusUpdate::from_event(&data);
        if items.is_empty() && status.is_empty() {
            return Ok(None);
        }
        Ok(Some(ListenerEvent::Messages {
            group,
            items,
            history: false,
            status,
        }))
    }

    fn decode_body(&self, frame: &Frame) -> Result<Value, ListenerError> {
        let body = frame.body_json()?;
        if body.get("encrypt").is_some() {
            decode_event_data(&body, self.cipher_key.as_deref())
        } else {
            Ok(body)
        }
    }

    fn alloc_req(&mut self) -> String {
        self.req_seq += 1;
        format!("req_{}", self.req_seq)
    }

    fn cursor_of(&self, cmd: u16, sub: u8) -> String {
        // `none_ids_queue` (518, 519) always starts at `"1"`.
        if matches!(cmd, 518 | 519) {
            return "1".to_owned();
        }
        self.cursors
            .get(&queue_key(cmd, sub))
            .cloned()
            .unwrap_or_else(|| "1".to_owned())
    }

    /// Remember `lastActionId` and ask for the next page when `more` is set.
    fn note_page(&mut self, cmd: u16, sub: u8, data: &Value) {
        if !self.pulling.contains(&(cmd, sub)) {
            return;
        }
        let root = data.get("data").filter(|v| v.is_object()).unwrap_or(data);
        let Some(id) = json_id(root.get("lastActionId")) else {
            return;
        };
        let key = queue_key(cmd, sub);
        // `none_ids_queue`: always resume from `"1"`, so the id lives only for
        // this connection. The same id must not request another page.
        let skip_store = matches!(cmd, 518 | 519);
        let previous = if skip_store {
            self.volatile.get(&key)
        } else {
            self.cursors.get(&key)
        };
        let advanced = previous.map(String::as_str) != Some(id.as_str());
        if !advanced {
            return;
        }
        if skip_store {
            self.volatile.insert(key, id.clone());
        } else {
            self.cursors.insert(key, id.clone());
            self.cursors_dirty = true;
        }
        if page_more(root.get("more")) {
            let req = self.alloc_req();
            self.outbound
                .push(offline_pull_frame(cmd, sub, false, &id, &req));
        }
    }

    fn enqueue_get_msg(&mut self, data: &Value) {
        let root = data.get("data").filter(|v| v.is_object()).unwrap_or(data);
        let Some(queue) = root.get("queueId").filter(|v| v.is_object()) else {
            return;
        };
        let Some(cmd) = json_u16(queue.get("cmd")) else {
            return;
        };
        let sub = json_u16(queue.get("subCmd")).unwrap_or(0) as u8;
        let Some(ids) = root.get("ids").and_then(Value::as_array) else {
            return;
        };
        if ids.is_empty() {
            return;
        }
        let last = self.cursor_of(cmd, sub);
        for chunk in ids.chunks(GET_MSG_BATCH) {
            let req = self.alloc_req();
            self.outbound.push(Frame::json(
                CMD_GET_MSG,
                0,
                &serde_json::json!({
                    "queueId": queue,
                    "ids": chunk,
                    "lastId": last,
                    "reqId": req,
                }),
            ));
        }
    }
}

fn queue_key(cmd: u16, sub: u8) -> String {
    format!("{cmd}_{sub}")
}

fn json_u16(value: Option<&Value>) -> Option<u16> {
    let value = value?;
    value
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .or_else(|| value.as_str()?.parse().ok())
}

fn json_id(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn page_more(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(on)) => *on,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(text)) => text == "1" || text.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn push_queue_list(out: &mut Vec<(u16, u8)>, value: &Value) {
    for key in ["qCmds", "ctrl_qCmds"] {
        let Some(list) = value.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in list {
            let Some(cmd) = json_u16(item.get("cmd")) else {
                continue;
            };
            let sub = json_u16(item.get("subCmd")).unwrap_or(0) as u8;
            if !out.contains(&(cmd, sub)) {
                out.push((cmd, sub));
            }
        }
    }
}

/// `doGetOffline`: `{first, reqId, lastId, preIds}` on the queue's own command.
pub fn offline_pull_frame(
    cmd: u16,
    sub_cmd: u8,
    first: bool,
    last_id: &str,
    req_id: &str,
) -> Frame {
    Frame::json(
        cmd,
        sub_cmd,
        &serde_json::json!({
            "first": first,
            "reqId": req_id,
            "lastId": last_id,
            "preIds": [],
        }),
    )
}

fn is_message_cmd(cmd: u16) -> bool {
    matches!(
        cmd,
        CMD_USER_MESSAGES
            | CMD_GROUP_MESSAGES
            | CMD_USER_CATCH_UP
            | CMD_GROUP_CATCH_UP
            | CMD_QUEUE_513
            | CMD_QUEUE_514
            | CMD_QUEUE_515
            | CMD_PUSH_MISS
            | CMD_GET_MSG
            | CMD_PRELOAD
            | CMD_QUEUE_710
            | CMD_QUEUE_711
            | CMD_QUEUE_713
            | CMD_QUEUE_715
    )
}

/// History batches (`510`/`513`/`571`/…). Live pushes `501`/`521` are handled apart
/// so a backlog does not notify once per message.
fn queued_messages(cmd: u16, data: &Value) -> Option<ListenerEvent> {
    if !is_message_cmd(cmd) {
        return None;
    }
    let (group, items) = split_messages(cmd, data);
    let status = crate::status::StatusUpdate::from_event(data);
    if items.is_empty() && status.is_empty() {
        return None;
    }
    Some(ListenerEvent::Messages {
        group,
        items,
        history: true,
        status,
    })
}

fn split_messages(cmd: u16, event: &Value) -> (bool, Vec<Value>) {
    let root = event.get("data").filter(|v| v.is_object()).unwrap_or(event);
    let group_msgs = array_of(root, "groupMsgs");
    let msgs = array_of(root, "msgs");
    let group_cmd = matches!(
        cmd,
        CMD_GROUP_CATCH_UP
            | CMD_GROUP_MESSAGES
            | CMD_GROUP_STATUS
            | CMD_GROUP_CLEAR_UNREAD
            | CMD_QUEUE_711
    );
    if group_cmd {
        if !group_msgs.is_empty() {
            return (true, group_msgs);
        }
        return (true, msgs);
    }
    if !group_msgs.is_empty() && msgs.is_empty() {
        return (true, group_msgs);
    }
    if !group_msgs.is_empty() && !msgs.is_empty() {
        // Mixed offline/preload frame carrying both 1-1 `msgs` and `groupMsgs`.
        // Mark group items explicitly so downstream parsers route them to `g_<id>`.
        let mut combined = msgs;
        for mut item in group_msgs {
            if let Some(obj) = item.as_object_mut() {
                obj.entry("isGroup".to_owned())
                    .or_insert_with(|| Value::from(1));
            }
            combined.push(item);
        }
        return (false, combined);
    }
    (false, msgs)
}

fn array_of(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn load_queue_cursors(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_queue_cursors(path: &Path, cursors: &HashMap<String, String>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(cursors) {
        let _ = std::fs::write(path, text);
    }
}

/// One typing indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typing {
    /// Conversation: the user id, or `g_<groupId>` for a group.
    pub thread_id: String,
    /// Who is typing.
    pub user_id: String,
    /// Whether the conversation is a group.
    pub group: bool,
}

/// `file_done` and `video_done` controls (`cmd 601`): `(fileId, url)` pairs.
/// Zalo PC's `_processFileAsyncDoneEvent` and `_processVideoAsyncDoneEvent`
/// read the same `fileId` + `data.url` shape.
#[must_use]
pub fn file_done_from_event(event: &Value) -> Vec<(String, String)> {
    let root = event.get("data").filter(|v| v.is_object()).unwrap_or(event);
    let Some(list) = root.get("controls").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in list {
        let ctrl = item
            .get("content")
            .filter(|v| v.is_object())
            .unwrap_or(item);
        let act = ctrl.get("act_type").and_then(Value::as_str);
        if act != Some("file_done") && act != Some("video_done") {
            continue;
        }
        let data = match ctrl.get("data") {
            Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
            Some(other) => other.clone(),
            None => Value::Null,
        };
        let id = ctrl.get("fileId").and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        });
        let url = data.get("url").and_then(Value::as_str).map(str::to_owned);
        if let (Some(id), Some(url)) = (id, url) {
            out.push((id, url));
        }
    }
    out
}

/// `{"data":{"actions":[{"act_type":"typing","act":"typing"|"gtyping","data":"…"}]}}`.
/// `data` is JSON, or JSON without the outer braces.
pub fn typing_from(event: &Value) -> Vec<Typing> {
    let root = event.get("data").filter(|v| v.is_object()).unwrap_or(event);
    let Some(actions) = root.get("actions").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for action in actions {
        if action.get("act_type").and_then(Value::as_str) != Some("typing") {
            continue;
        }
        let group = action.get("act").and_then(Value::as_str) == Some("gtyping");
        let data = match action.get("data") {
            Some(Value::String(text)) => serde_json::from_str::<Value>(text)
                .ok()
                .filter(Value::is_object)
                .or_else(|| serde_json::from_str(&format!("{{{text}}}")).ok()),
            Some(other) if other.is_object() => Some(other.clone()),
            _ => None,
        };
        let Some(data) = data else { continue };
        let text = |key: &str| match data.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        let user_id = text("uid");
        if user_id.is_empty() {
            continue;
        }
        let thread_id = if group {
            let gid = text("gid");
            if gid.is_empty() {
                continue;
            }
            format!("g_{gid}")
        } else {
            user_id.clone()
        };
        out.push(Typing {
            thread_id,
            user_id,
            group,
        });
    }
    out
}

/// Request for messages after `last_id` (`cmd 510` direct, `511` group).
pub fn catch_up_frame(group: bool, last_id: &str) -> Frame {
    Frame::json(
        if group {
            CMD_GROUP_CATCH_UP
        } else {
            CMD_USER_CATCH_UP
        },
        1,
        &serde_json::json!({ "first": true, "lastId": last_id, "preIds": [] }),
    )
}

/// Newest message id seen per kind, kept across reconnects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LastIds {
    user: Option<String>,
    group: Option<String>,
}

impl LastIds {
    fn record(&mut self, group: bool, items: &[Value]) {
        let slot = if group {
            &mut self.group
        } else {
            &mut self.user
        };
        for item in items {
            let id = match item.get("msgId") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => continue,
            };
            let newer = match slot.as_deref() {
                None => true,
                Some(old) => (id.len(), id.as_str()) > (old.len(), old),
            };
            if newer {
                *slot = Some(id);
            }
        }
    }
}

/// Message array of a 501/521 event (`msgs` or `groupMsgs`, at the root or under `data`).
pub fn message_items(event: &Value, group: bool) -> Vec<Value> {
    let root = event.get("data").filter(|v| v.is_object()).unwrap_or(event);
    let keys: &[&str] = if group {
        &["groupMsgs", "msgs"]
    } else {
        &["msgs"]
    };
    keys.iter()
        .find_map(|key| root.get(*key).and_then(Value::as_array))
        .cloned()
        .unwrap_or_default()
}

/// Decodes `{"data": …, "encrypt": n}` into JSON.
///
/// # Errors
///
/// Returns [`ListenerError`] for bad base64, a missing or wrong key, or a body
/// that does not inflate to JSON.
pub fn decode_event_data(body: &Value, cipher_key: Option<&str>) -> Result<Value, ListenerError> {
    let encrypt = body.get("encrypt").and_then(Value::as_i64).unwrap_or(0);
    let raw = body
        .get("data")
        .ok_or_else(|| ListenerError::Unexpected("event has no data".to_owned()))?;
    if encrypt == 0 {
        return match raw {
            Value::String(text) => Ok(serde_json::from_str(text)?),
            other => Ok(other.clone()),
        };
    }
    let text = raw
        .as_str()
        .ok_or_else(|| ListenerError::Unexpected("encoded event data is not a string".into()))?;
    let bytes = base64_decode(&percent_decode(text))?;
    let plain = if encrypt == 1 {
        bytes
    } else {
        let key = cipher_key.ok_or(ListenerError::NoCipherKey)?;
        gcm_decrypt(&base64_decode(key)?, &bytes)?
    };
    let json = if encrypt == 3 {
        plain
    } else {
        inflate(&plain)?
    };
    Ok(serde_json::from_slice(&json)?)
}

/// AES-GCM with a 16-byte IV: `iv(16) || aad(16) || ciphertext || tag(16)`.
fn gcm_decrypt(key: &[u8], blob: &[u8]) -> Result<Vec<u8>, ListenerError> {
    if blob.len() < 48 {
        return Err(ListenerError::Decrypt);
    }
    let nonce = Nonce::<U16>::from_slice(&blob[..16]);
    let payload = Payload {
        msg: &blob[32..],
        aad: &blob[16..32],
    };
    match key.len() {
        16 => AesGcm::<Aes128, U16>::new_from_slice(key)
            .map_err(|_| ListenerError::Decrypt)?
            .decrypt(nonce, payload),
        32 => AesGcm::<Aes256, U16>::new_from_slice(key)
            .map_err(|_| ListenerError::Decrypt)?
            .decrypt(nonce, payload),
        _ => return Err(ListenerError::Decrypt),
    }
    .map_err(|_| ListenerError::Decrypt)
}

/// zlib or gzip (detected by the magic bytes), like `pako.inflate`.
fn inflate(bytes: &[u8]) -> Result<Vec<u8>, ListenerError> {
    let mut out = Vec::new();
    if bytes.starts_with(&[0x1f, 0x8b]) {
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut out)?;
    } else {
        flate2::read::ZlibDecoder::new(bytes).read_to_end(&mut out)?;
    }
    Ok(out)
}

/// `decodeURIComponent` for the `%XX` escapes Zalo puts in base64 (`%2B`, `%2F`, `%3D`).
fn percent_decode(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Ping period from a getServerInfo / settings response
/// (`settings.features.socket.ping_interval`, milliseconds).
pub fn ping_interval_from(server_info: &Value) -> Option<Duration> {
    let root = server_info.get("data").unwrap_or(server_info);
    let settings = root.get("settings").or_else(|| root.get("setttings"))?;
    let ms = settings
        .pointer("/features/socket/ping_interval")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))?;
    (ms >= 1000).then(|| Duration::from_millis(ms))
}

/// `zpw_ws` endpoints from a getLoginInfo or getServerInfo response.
pub fn ws_endpoints_from(value: &Value) -> Vec<String> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    root.get("zpw_ws")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|url| url.starts_with("wss://"))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Everything needed to open the socket.
#[derive(Clone)]
pub struct ListenerConfig {
    /// `wss://` endpoints, tried in order.
    pub endpoints: Vec<String>,
    /// `Cookie` header value (carries `zpw_sek`). Never logged.
    pub cookie: String,
    /// `User-Agent` header value.
    pub user_agent: String,
    /// Keep-alive period.
    pub ping_interval: Duration,
    /// Newest direct-message id the caller already has. Used only when the
    /// auth payload does not list `qCmds`; otherwise the listener resumes each
    /// queue from [`Self::cursor_path`].
    pub since_user: Option<String>,
    /// Same for group messages (`cmd 511`).
    pub since_group: Option<String>,
    /// `queue-cursors.json`: last `lastActionId` per `"cmd_sub"` queue.
    /// `None` keeps the cursors in memory for this process only.
    pub cursor_path: Option<std::path::PathBuf>,
    /// Optional cap on consecutive disconnects before reaching `Ready`.
    /// When reached, [`run_listener_with`] returns [`ListenerError::RefreshNeeded`]
    /// so the caller can refresh `getLoginInfo` cookies and endpoints.
    pub max_unready_retries: Option<u32>,
}

impl std::fmt::Debug for ListenerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerConfig")
            .field("endpoints", &self.endpoints)
            .field("cookie", &"<hidden>")
            .field("user_agent", &self.user_agent)
            .field("ping_interval", &self.ping_interval)
            .field("since_user", &self.since_user)
            .field("since_group", &self.since_group)
            .field("cursor_path", &self.cursor_path)
            .field("max_unready_retries", &self.max_unready_retries)
            .finish()
    }
}

/// Returns `true` when `reason` is a common transient network/TLS disconnect
/// (e.g. peer closed TCP without TLS `close_notify`, reset, or timeout).
#[must_use]
pub fn is_transient_disconnect(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    [
        "close_notify",
        "unexpected-eof",
        "unexpected eof",
        "connection reset",
        "broken pipe",
        "timed out",
        "socket ended",
        "dns",
        "failed to lookup",
        "network is unreachable",
        "temporary failure",
        "eof",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Builds `endpoint?zpw_ver=…&zpw_type=…&t=…`.
///
/// # Errors
///
/// Returns [`ListenerError::Url`] when the endpoint does not parse.
pub fn socket_url(endpoint: &str, now_ms: i64) -> Result<String, ListenerError> {
    let mut url = Url::parse(endpoint).map_err(|e| ListenerError::Url(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("zpw_ver", &client_version())
        .append_pair("zpw_type", &api_type().to_string())
        .append_pair("t", &now_ms.to_string());
    Ok(url.to_string())
}

/// Runs the listener until `on_event` returns `false` or another client
/// replaces the session. Drops reconnect with backoff (2 s doubling to 60 s,
/// reset after each successful handshake).
///
/// # Errors
///
/// Returns [`ListenerError::Url`] when no endpoint parses. Socket errors are
/// reported as [`ListenerEvent::Disconnected`] and retried.
pub async fn run_listener<F>(config: ListenerConfig, on_event: F) -> Result<(), ListenerError>
where
    F: FnMut(ListenerEvent) -> bool,
{
    run_listener_with(config, None, on_event).await
}

/// [`run_listener`] that also writes the frames sent on `outgoing` to the
/// socket (requests such as sync `cmd 590`). Frames sent while the socket is
/// reconnecting wait in the channel.
///
/// # Errors
///
/// Same as [`run_listener`].
pub async fn run_listener_with<F>(
    config: ListenerConfig,
    mut outgoing: Option<tokio::sync::mpsc::UnboundedReceiver<Frame>>,
    mut on_event: F,
) -> Result<(), ListenerError>
where
    F: FnMut(ListenerEvent) -> bool,
{
    let endpoints = if config.endpoints.is_empty() {
        vec![DEFAULT_WS_ENDPOINT.to_owned()]
    } else {
        config.endpoints.clone()
    };
    let mut backoff = Duration::from_secs(2);
    let mut attempt = 0_usize;
    let mut consecutive_unready = 0_u32;
    let mut last_ids = LastIds {
        user: config.since_user.clone(),
        group: config.since_group.clone(),
    };
    let mut cursors = config
        .cursor_path
        .as_deref()
        .map(load_queue_cursors)
        .unwrap_or_default();
    loop {
        let endpoint = &endpoints[attempt % endpoints.len()];
        attempt += 1;
        match run_once(
            &config,
            endpoint,
            &mut last_ids,
            &mut cursors,
            &mut outgoing,
            &mut on_event,
        )
        .await
        {
            Ok(Outcome::Stop) => return Ok(()),
            Ok(Outcome::Replaced) => {
                on_event(ListenerEvent::Replaced);
                return Ok(());
            }
            Ok(Outcome::Dropped { was_ready, reason }) => {
                if was_ready {
                    backoff = Duration::from_secs(2);
                    consecutive_unready = 0;
                } else {
                    consecutive_unready = consecutive_unready.saturating_add(1);
                }
                if !on_event(ListenerEvent::Disconnected(reason)) {
                    return Ok(());
                }
                if config
                    .max_unready_retries
                    .is_some_and(|limit| limit > 0 && consecutive_unready >= limit)
                {
                    return Err(ListenerError::RefreshNeeded);
                }
            }
            Err(ListenerError::Url(e)) => return Err(ListenerError::Url(e)),
            Err(err) => {
                consecutive_unready = consecutive_unready.saturating_add(1);
                if !on_event(ListenerEvent::Disconnected(err.to_string())) {
                    return Ok(());
                }
                if config
                    .max_unready_retries
                    .is_some_and(|limit| limit > 0 && consecutive_unready >= limit)
                {
                    return Err(ListenerError::RefreshNeeded);
                }
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

enum Outcome {
    Stop,
    Replaced,
    Dropped { was_ready: bool, reason: String },
}

async fn run_once<F>(
    config: &ListenerConfig,
    endpoint: &str,
    last_ids: &mut LastIds,
    cursors: &mut HashMap<String, String>,
    outgoing: &mut Option<tokio::sync::mpsc::UnboundedReceiver<Frame>>,
    on_event: &mut F,
) -> Result<Outcome, ListenerError>
where
    F: FnMut(ListenerEvent) -> bool,
{
    let url = socket_url(endpoint, unix_ms())?;
    let mut request = url.as_str().into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("Origin", HeaderValue::from_static("https://chat.zalo.me"));
    if let Ok(value) = HeaderValue::from_str(&config.user_agent) {
        headers.insert("User-Agent", value);
    }
    if let Ok(value) = HeaderValue::from_str(&config.cookie) {
        headers.insert("Cookie", value);
    }
    headers.insert("Cache-Control", HeaderValue::from_static("no-cache"));
    headers.insert("Pragma", HeaderValue::from_static("no-cache"));

    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    let (mut sink, mut stream) = socket.split();
    if !on_event(ListenerEvent::Connected) {
        let _ = sink.close().await;
        return Ok(Outcome::Stop);
    }
    let mut decoder = EventDecoder::with_cursors(cursors.clone());
    let mut ping = tokio::time::interval(config.ping_interval);
    ping.tick().await;
    let mut last_rx = tokio::time::Instant::now();
    let stale_after = config
        .ping_interval
        .saturating_mul(2)
        .max(Duration::from_secs(45));
    loop {
        tokio::select! {
            incoming = stream.next() => {
                let message = match incoming {
                    Some(Ok(message)) => {
                        last_rx = tokio::time::Instant::now();
                        message
                    }
                    Some(Err(err)) => return Ok(Outcome::Dropped {
                        was_ready: decoder.is_ready(),
                        reason: err.to_string(),
                    }),
                    None => return Ok(Outcome::Dropped {
                        was_ready: decoder.is_ready(),
                        reason: "socket ended".to_owned(),
                    }),
                };
                let bytes = match message {
                    Message::Binary(bytes) => bytes.to_vec(),
                    Message::Text(text) => text.as_bytes().to_vec(),
                    Message::Close(frame) => {
                        let code = frame.as_ref().map(|f| u16::from(f.code));
                        if code == Some(CLOSE_DUPLICATE) {
                            return Ok(Outcome::Replaced);
                        }
                        return Ok(Outcome::Dropped {
                            was_ready: decoder.is_ready(),
                            reason: format!("close {}", code.map_or("-".to_owned(), |c| c.to_string())),
                        });
                    }
                    _ => continue,
                };
                let Ok(frame) = Frame::decode(&bytes) else { continue };
                // A single bad event must not tear the connection down.
                let parsed = decoder.handle(&frame);
                let mut extra = decoder.take_outbound();
                if decoder.cursors_dirty() {
                    *cursors = decoder.cursors().clone();
                    decoder.clear_cursors_dirty();
                    if let Some(path) = &config.cursor_path {
                        save_queue_cursors(path, cursors);
                    }
                }
                let Ok(event) = parsed else { continue };
                let Some(event) = event else {
                    if let Err(err) = send_frames(&mut sink, extra).await {
                        return Ok(Outcome::Dropped {
                            was_ready: decoder.is_ready(),
                            reason: err,
                        });
                    }
                    continue;
                };
                if event == ListenerEvent::Replaced {
                    let _ = sink.close().await;
                    return Ok(Outcome::Replaced);
                }
                if event == ListenerEvent::Ready && extra.is_empty() {
                    // Auth had no `qCmds`. Fall back to the direct 510/511 resume.
                    let pending = [(false, last_ids.user.clone()), (true, last_ids.group.clone())];
                    for (group, last) in pending {
                        let Some(last) = last else { continue };
                        extra.push(catch_up_frame(group, &last));
                    }
                }
                if let Err(err) = send_frames(&mut sink, extra).await {
                    return Ok(Outcome::Dropped { was_ready: true, reason: err });
                }
                if let ListenerEvent::Messages { group, items, .. } = &event {
                    last_ids.record(*group, items);
                }
                if !on_event(event) {
                    let _ = sink.close().await;
                    return Ok(Outcome::Stop);
                }
            }
            Some(frame) = next_outgoing(outgoing, decoder.is_ready()) => {
                if let Err(err) = sink.send(Message::binary(frame.encode())).await {
                    return Ok(Outcome::Dropped { was_ready: true, reason: err.to_string() });
                }
            }
            _ = ping.tick() => {
                if decoder.is_ready() {
                    if last_rx.elapsed() >= stale_after {
                        let _ = sink.close().await;
                        return Ok(Outcome::Dropped {
                            was_ready: true,
                            reason: "connection timed out".to_owned(),
                        });
                    }
                    let frame = Frame::ping(unix_ms());
                    if let Err(err) = sink.send(Message::binary(frame.encode())).await {
                        return Ok(Outcome::Dropped {
                            was_ready: true,
                            reason: err.to_string(),
                        });
                    }
                }
            }
        }
    }
}

async fn send_frames<S>(sink: &mut S, frames: Vec<Frame>) -> Result<(), String>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    for frame in frames {
        sink.send(Message::binary(frame.encode()))
            .await
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// Next frame to send, once the handshake is done; pending forever otherwise.
async fn next_outgoing(
    outgoing: &mut Option<tokio::sync::mpsc::UnboundedReceiver<Frame>>,
    ready: bool,
) -> Option<Frame> {
    match outgoing {
        Some(rx) if ready => rx.recv().await,
        _ => std::future::pending().await,
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_done_controls_are_parsed() {
        let event = serde_json::json!({"controls": [
            {"content": {"act_type": "file_done", "fileId": 123, "data": "{\"url\":\"https://f.example/a.txt\"}"}},
            {"content": {"act_type": "video_done", "fileId": "vid-9", "data": {"url": "https://v.example/a.mp4"}}},
            {"content": {"act_type": "voip", "data": "{}"}}
        ]});
        assert_eq!(
            file_done_from_event(&event),
            vec![
                ("123".to_owned(), "https://f.example/a.txt".to_owned()),
                ("vid-9".to_owned(), "https://v.example/a.mp4".to_owned()),
            ]
        );
    }
    use aes_gcm::aead::Aead;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use serde_json::json;
    use std::io::Write;
    use zalo_protocol::base64_encode;

    fn zlib(bytes: &[u8]) -> Vec<u8> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(bytes).expect("write");
        enc.finish().expect("finish")
    }

    fn seal(key: &[u8], plain: &[u8]) -> Vec<u8> {
        let iv = [3_u8; 16];
        let aad = [9_u8; 16];
        let cipher = AesGcm::<Aes256, U16>::new_from_slice(key).expect("key");
        let sealed = cipher
            .encrypt(
                Nonce::<U16>::from_slice(&iv),
                Payload {
                    msg: plain,
                    aad: &aad,
                },
            )
            .expect("seal");
        let mut out = iv.to_vec();
        out.extend_from_slice(&aad);
        out.extend_from_slice(&sealed);
        out
    }

    #[test]
    fn frame_header_roundtrips_little_endian() {
        let frame = Frame::json(521, 0, &json!({"a": 1}));
        let bytes = frame.encode();
        assert_eq!(&bytes[..4], &[1, 0x09, 0x02, 0]);
        assert_eq!(Frame::decode(&bytes).expect("decode"), frame);
        assert!(matches!(
            Frame::decode(&[1, 2]),
            Err(ListenerError::ShortFrame)
        ));
    }

    #[test]
    fn ping_frame_carries_event_id() {
        let frame = Frame::ping(1234);
        assert_eq!((frame.cmd, frame.sub_cmd), (CMD_PING, 1));
        assert_eq!(frame.body_json().expect("json"), json!({"eventId": 1234}));
    }

    #[test]
    fn decodes_plain_and_zlib_events() {
        let plain = json!({"encrypt": 0, "data": "{\"msgs\":[]}"});
        assert_eq!(
            decode_event_data(&plain, None).expect("plain"),
            json!({"msgs": []})
        );
        let packed = base64_encode(&zlib(br#"{"data":{"msgs":[{"msgId":"1"}]}}"#));
        let body = json!({"encrypt": 1, "data": packed});
        let value = decode_event_data(&body, None).expect("zlib");
        assert_eq!(message_items(&value, false).len(), 1);
    }

    #[test]
    fn decodes_gcm_events_with_percent_escaped_base64() {
        let key = [5_u8; 32];
        let event =
            br#"{"data":{"groupMsgs":[{"msgId":"7","idTo":"55","uidFrom":"9","content":"chao"}]}}"#;
        let blob = seal(&key, &zlib(event));
        let encoded = base64_encode(&blob)
            .replace('+', "%2B")
            .replace('/', "%2F")
            .replace('=', "%3D");
        let body = json!({"encrypt": 2, "data": encoded});
        let key_b64 = base64_encode(&key);
        let value = decode_event_data(&body, Some(&key_b64)).expect("gcm");
        let items = message_items(&value, true);
        assert_eq!(items[0]["content"], "chao");

        let raw = json!({"encrypt": 3, "data": base64_encode(&seal(&key, br#"{"x":1}"#))});
        assert_eq!(
            decode_event_data(&raw, Some(&key_b64)).expect("gcm raw"),
            json!({"x": 1})
        );
        assert!(matches!(
            decode_event_data(&body, None),
            Err(ListenerError::NoCipherKey)
        ));
        let wrong = base64_encode(&[6_u8; 32]);
        assert!(matches!(
            decode_event_data(&body, Some(&wrong)),
            Err(ListenerError::Decrypt)
        ));
    }

    #[test]
    fn decoder_needs_handshake_then_emits_messages() {
        let key = [1_u8; 32];
        let mut decoder = EventDecoder::new();
        assert!(!decoder.is_ready());
        let hello = Frame::json(1, 1, &json!({"key": base64_encode(&key)}));
        assert_eq!(
            decoder.handle(&hello).expect("hello"),
            Some(ListenerEvent::Ready)
        );
        let event =
            br#"{"data":{"msgs":[{"msgId":"1","uidFrom":"42","idTo":"0","content":"hi"}]}}"#;
        let body = json!({"encrypt": 2, "data": base64_encode(&seal(&key, &zlib(event)))});
        let frame = Frame::json(CMD_USER_MESSAGES, 0, &body);
        match decoder.handle(&frame).expect("msgs") {
            Some(ListenerEvent::Messages {
                group,
                items,
                history,
                ..
            }) => {
                assert!(!group);
                assert!(!history);
                assert_eq!(items[0]["uidFrom"], "42");
            }
            other => panic!("unexpected {other:?}"),
        }
        let status = Frame::json(
            CMD_USER_CLEAR_UNREAD,
            0,
            &json!({"encrypt": 0, "data": {"clearUnreads": [{"idTo": "42", "isGroup": 0, "lastMsgId": "9"}]}}),
        );
        assert!(matches!(
            decoder.handle(&status).expect("status"),
            Some(ListenerEvent::Status(s)) if s.read[0].thread_id == "42"
        ));
        let empty = Frame::json(
            CMD_USER_MESSAGES,
            0,
            &json!({"encrypt": 0, "data": {"msgs": []}}),
        );
        assert_eq!(decoder.handle(&empty).expect("empty"), None);
        let dup = Frame::json(CMD_DUPLICATE, 0, &Value::Null);
        assert_eq!(
            decoder.handle(&dup).expect("dup"),
            Some(ListenerEvent::Replaced)
        );
        let react = Frame::json(612, 0, &json!({"encrypt": 0, "data": {"reacts": []}}));
        assert!(matches!(
            decoder.handle(&react).expect("react"),
            Some(ListenerEvent::Event { cmd: 612, .. })
        ));
    }

    #[test]
    fn catch_up_and_typing_frames() {
        let frame = catch_up_frame(true, "7001");
        assert_eq!((frame.cmd, frame.sub_cmd), (CMD_GROUP_CATCH_UP, 1));
        assert_eq!(
            frame.body_json().expect("json"),
            json!({"first": true, "lastId": "7001", "preIds": []})
        );
        let mut decoder = EventDecoder::new();
        let reply = Frame::json(
            CMD_USER_CATCH_UP,
            1,
            &json!({"encrypt": 0, "data": {"msgs": [{"msgId": "9"}]}}),
        );
        assert!(matches!(
            decoder.handle(&reply).expect("catch up"),
            Some(ListenerEvent::Messages {
                history: true,
                group: false,
                ..
            })
        ));

        let typing = json!({"data": {"actions": [
            {"act_type": "typing", "act": "typing", "data": "\"uid\":\"42\",\"ts\":\"1\""},
            {"act_type": "typing", "act": "gtyping", "data": "{\"uid\":\"43\",\"gid\":\"77\"}"},
            {"act_type": "other", "act": "x", "data": "{}"}
        ]}});
        let list = typing_from(&typing);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].thread_id, "42");
        assert_eq!(list[1].thread_id, "g_77");
        assert!(list[1].group);
        let frame = Frame::json(CMD_TYPING, 0, &json!({"encrypt": 0, "data": typing}));
        assert!(matches!(
            decoder.handle(&frame).expect("typing"),
            Some(ListenerEvent::Typing(list)) if list.len() == 2
        ));
    }

    #[test]
    fn last_ids_keep_the_newest_numeric_id() {
        let mut ids = LastIds::default();
        ids.record(
            false,
            &[
                json!({"msgId": "99"}),
                json!({"msgId": 1000}),
                json!({"msgId": "998"}),
            ],
        );
        assert_eq!(ids.user.as_deref(), Some("1000"));
        assert_eq!(ids.group, None);
    }

    #[test]
    fn reads_endpoints_ping_and_builds_url() {
        let info = json!({"data": {
            "zpw_ws": ["wss://ws2-msg.chat.zalo.me", "http://bad"],
            "settings": {"features": {"socket": {"ping_interval": 20000}}}
        }});
        assert_eq!(ws_endpoints_from(&info), vec!["wss://ws2-msg.chat.zalo.me"]);
        assert_eq!(ping_interval_from(&info), Some(Duration::from_secs(20)));
        let typo = json!({"setttings": {"features": {"socket": {"ping_interval": "45000"}}}});
        assert_eq!(ping_interval_from(&typo), Some(Duration::from_secs(45)));
        let url = socket_url("wss://ws2-msg.chat.zalo.me", 99).expect("url");
        assert_eq!(
            url,
            format!(
                "wss://ws2-msg.chat.zalo.me/?zpw_ver={}&zpw_type={}&t=99",
                client_version(),
                api_type()
            )
        );
    }

    #[test]
    fn percent_decode_handles_escapes_and_plain_text() {
        assert_eq!(percent_decode("ab%2Bc%3D%3D"), "ab+c==");
        assert_eq!(percent_decode("abc"), "abc");
        assert_eq!(percent_decode("a%"), "a%");
    }

    #[test]
    fn config_debug_hides_cookie() {
        let config = ListenerConfig {
            endpoints: vec![],
            cookie: "zpw_sek=secret".to_owned(),
            user_agent: "ua".to_owned(),
            ping_interval: DEFAULT_PING_INTERVAL,
            since_user: None,
            since_group: None,
            cursor_path: None,
            max_unready_retries: Some(3),
        };
        assert!(!format!("{config:?}").contains("secret"));
        assert!(is_transient_disconnect(
            "IO error: peer closed connection without sending TLS close_notify: https://docs.rs/rustls/latest/rustls/manual/_03_howto/index.html#unexpected-eof"
        ));
        assert!(is_transient_disconnect("connection timed out"));
        assert!(!is_transient_disconnect("Zalo returned -1: session expired"));
    }

    #[test]
    fn auth_queues_pull_offline_and_has_msg_fetches_bodies() {
        let mut decoder = EventDecoder::with_cursors(std::collections::HashMap::from([(
            "510_0".to_owned(),
            "42".to_owned(),
        )]));
        let auth = Frame::json(
            CMD_HANDSHAKE,
            1,
            &json!({
                "key": base64_encode(&[7_u8; 32]),
                "error_code": 0,
                "encrypt": 0,
                "data": {
                    "qCmds": [{"cmd": 510, "subCmd": 0}, {"cmd": 513, "subCmd": 0}, {"cmd": 571, "subCmd": 0}],
                    "ctrl_qCmds": [{"cmd": 603, "subCmd": 0}]
                }
            }),
        );
        assert_eq!(
            decoder.handle(&auth).expect("auth"),
            Some(ListenerEvent::Ready)
        );
        let pulls = decoder.take_outbound();
        assert_eq!(pulls.len(), 4);
        assert_eq!(pulls[0].cmd, 510);
        assert_eq!(
            pulls[0].body_json().expect("body")["lastId"],
            json!("42"),
            "resume the saved action id"
        );
        assert_eq!(pulls[1].body_json().expect("body")["lastId"], json!("1"));
        assert_eq!(pulls[1].cmd, CMD_QUEUE_513);
        assert_eq!(pulls[3].cmd, CMD_PULL_CTRL);
        assert!(pulls
            .iter()
            .all(|f| f.body_json().expect("body")["preIds"] == json!([])));

        let ids: Vec<_> = (0..21)
            .map(|n| json!({"actionId": n, "msgId": n + 100}))
            .collect();
        let has = Frame::json(
            CMD_HAS_MSG,
            0,
            &json!({"encrypt": 0, "data": {"queueId": {"cmd": 510, "subCmd": 0}, "ids": ids}}),
        );
        assert_eq!(decoder.handle(&has).expect("has"), None);
        let gets = decoder.take_outbound();
        assert_eq!(gets.len(), 2, "20 ids then the remainder");
        assert_eq!(gets[0].cmd, CMD_GET_MSG);
        let body = gets[0].body_json().expect("get");
        assert_eq!(body["ids"].as_array().expect("ids").len(), 20);
        assert_eq!(body["lastId"], json!("42"));
        assert_eq!(
            gets[1].body_json().expect("rest")["ids"]
                .as_array()
                .expect("ids")
                .len(),
            1
        );
    }

    #[test]
    fn offline_page_advances_once_and_queues_are_messages() {
        let mut decoder = EventDecoder::new();
        let auth = Frame::json(
            CMD_HANDSHAKE,
            1,
            &json!({
                "key": base64_encode(&[4_u8; 32]),
                "error_code": 0,
                "encrypt": 0,
                "data": {"qCmds": [{"cmd": CMD_QUEUE_514, "subCmd": 0}]}
            }),
        );
        decoder.handle(&auth).expect("auth");
        assert_eq!(decoder.take_outbound().len(), 1);

        let page = Frame::json(
            CMD_QUEUE_514,
            0,
            &json!({"encrypt": 0, "data": {
                "more": 1,
                "lastActionId": "77",
                "groupMsgs": [{"msgId": "9", "content": "cu"}]
            }}),
        );
        match decoder.handle(&page).expect("page") {
            Some(ListenerEvent::Messages {
                group,
                history,
                items,
                ..
            }) => {
                assert!(group);
                assert!(history);
                assert_eq!(items[0]["content"], "cu");
            }
            other => panic!("unexpected {other:?}"),
        }
        let next = decoder.take_outbound();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].body_json().expect("next")["first"], json!(false));
        assert_eq!(next[0].body_json().expect("next")["lastId"], json!("77"));
        assert!(decoder.cursors_dirty());
        assert_eq!(
            decoder.cursors().get("514_0").map(String::as_str),
            Some("77")
        );

        let stuck = Frame::json(
            CMD_QUEUE_514,
            0,
            &json!({"encrypt": 0, "data": {"more": true, "lastActionId": "77"}}),
        );
        assert_eq!(decoder.handle(&stuck).expect("stuck"), None);
        assert!(
            decoder.take_outbound().is_empty(),
            "same action id must not loop"
        );
    }

    #[test]
    fn none_ids_queue_pages_once_and_is_not_stored() {
        let mut decoder = EventDecoder::new();
        let auth = Frame::json(
            CMD_HANDSHAKE,
            1,
            &json!({
                "key": base64_encode(&[5_u8; 32]),
                "error_code": 0,
                "encrypt": 0,
                "data": {"qCmds": [{"cmd": 518, "subCmd": 0}]}
            }),
        );
        decoder.handle(&auth).expect("auth");
        let first = decoder.take_outbound();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].body_json().expect("pull")["lastId"], json!("1"));

        let page = Frame::json(
            518,
            0,
            &json!({"encrypt": 0, "data": {"more": 1, "lastActionId": "9"}}),
        );
        decoder.handle(&page).expect("page");
        let next = decoder.take_outbound();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].body_json().expect("next")["lastId"], json!("9"));
        assert!(!decoder.cursors_dirty());
        assert!(decoder.cursors().get("518_0").is_none());

        let stuck = Frame::json(
            518,
            0,
            &json!({"encrypt": 0, "data": {"more": true, "lastActionId": "9"}}),
        );
        decoder.handle(&stuck).expect("stuck");
        assert!(decoder.take_outbound().is_empty());
        assert!(decoder.cursors().get("518_0").is_none());
    }

    #[test]
    fn miss_and_get_msg_bodies_are_history() {
        let mut decoder = EventDecoder::new();
        let frame = Frame::json(
            CMD_PUSH_MISS,
            0,
            &json!({"encrypt": 0, "data": {"msgs": [{"msgId": "3", "content": "lo"}]}}),
        );
        match decoder.handle(&frame).expect("miss") {
            Some(ListenerEvent::Messages {
                group,
                history,
                items,
                ..
            }) => {
                assert!(!group);
                assert!(history);
                assert_eq!(items[0]["content"], "lo");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn queue_cursors_roundtrip_on_disk() {
        let path =
            std::env::temp_dir().join(format!("zalo-queue-cursors-{}.json", std::process::id()));
        let mut map = std::collections::HashMap::new();
        map.insert("510_0".to_owned(), "99".to_owned());
        save_queue_cursors(&path, &map);
        assert_eq!(
            load_queue_cursors(&path).get("510_0").map(String::as_str),
            Some("99")
        );
        let _ = std::fs::remove_file(&path);
    }
}
