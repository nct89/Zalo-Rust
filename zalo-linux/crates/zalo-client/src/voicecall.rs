//! 1-1 `/api/voicecall/*` from Zalo PC `compact3178.js` / `compact1913.js`.
//!
//! The native engine emits `sendSignal` frames. The renderer
//! (`handleSendSignal` in `compact1913.js`) turns those into GET requests on
//! the `voice_call` host. The HTTP body comes back as `recvSignal`
//! (`handleRecvSignal` / compact3545 `kCOK` `r.data`). Incoming 1-1 calls
//! arrive as WS `cmd 601`/`603` controls with `act_type: "voip"`. Payload
//! builders are tested without the network.

use super::api::{ClientError, ZaloApi};
use serde_json::{json, Value};

/// Production fallback when `zpw_service_map_v3.voice_call` is missing.
/// Live hosts come from getLoginInfo.
pub const VOICE_CALL_FALLBACK: &str = "https://tt-voicecall-wpa.chat.zalo.me";

/// `sendSignal` 401 → [`/api/voicecall/requestcall`].
pub const SIG_REQUEST_CALL: i64 = 401;
/// `sendSignal` 416 → [`/api/voicecall/request`].
pub const SIG_SEND_REQUEST: i64 = 416;
/// `sendSignal` 407 → [`/api/voicecall/ringring`].
pub const SIG_RING: i64 = 407;
/// `sendSignal` 408 → [`/api/voicecall/answerack`].
pub const SIG_ANSWER_ACK: i64 = 408;
/// `sendSignal` 402 → [`/api/voicecall/answer`].
pub const SIG_ANSWER: i64 = 402;
/// `sendSignal` 405 → [`/api/voicecall/cancel`].
pub const SIG_CANCEL: i64 = 405;
/// `sendSignal` 406 → [`/api/voicecall/logendcall`].
pub const SIG_LOG_END: i64 = 406;
/// `sendSignal` 409 → [`/api/voicecall/endcall`].
pub const SIG_END: i64 = 409;
/// `sendSignal` 411 → [`/api/voicecall/holdreq`].
pub const SIG_HOLD_REQ: i64 = 411;
/// `sendSignal` 412 → [`/api/voicecall/holdresp`].
pub const SIG_HOLD_RESP: i64 = 412;
/// `sendSignal` 413 → [`/api/voicecall/unholdreq`].
pub const SIG_UNHOLD_REQ: i64 = 413;
/// `sendSignal` 414 → [`/api/voicecall/unholdresp`].
pub const SIG_UNHOLD_RESP: i64 = 414;
/// `sendSignal` 12006 → [`/api/voicecall/conf`].
pub const SIG_CONF: i64 = 12006;
/// Group call HTTP commands. The engine already builds the JsonCpp body.
pub const SIG_GROUP_REQUEST_CALL: i64 = 12433;
/// `sendSignal` 12434 → [`/api/voicecall/group/request`].
pub const SIG_GROUP_REQUEST: i64 = 12434;
/// `sendSignal` 12436 → [`/api/voicecall/group/answer`].
pub const SIG_GROUP_ANSWER: i64 = 12436;
/// `sendSignal` 12437 → [`/api/voicecall/group/cancel`].
pub const SIG_GROUP_CANCEL: i64 = 12437;
/// `sendSignal` 12438 → [`/api/voicecall/group/endcall`].
pub const SIG_GROUP_END: i64 = 12438;
/// `sendSignal` 12439 → [`/api/voicecall/group/ringring`].
pub const SIG_GROUP_RING: i64 = 12439;
/// `sendSignal` 12446 → [`/api/voicecall/group/finish`].
pub const SIG_GROUP_FINISH: i64 = 12446;
/// `sendSignal` 12097 → [`/api/voicecall/group/ping`].
pub const SIG_GROUP_PING: i64 = 12097;
/// `sendSignal` 12098 → [`/api/voicecall/group/broadcast`].
pub const SIG_GROUP_BROADCAST: i64 = 12098;
/// `sendSignal` 12044 → [`/api/voicecall/group/sharescreen`].
pub const SIG_GROUP_SHARE: i64 = 12044;
/// `sendSignal` 12064 → [`/api/voicecall/requestsharescreen`].
pub const SIG_SHARE_SCREEN: i64 = 12064;

fn clone_field(data: &Value, key: &str) -> Value {
    data.get(key).cloned().unwrap_or(Value::Null)
}

fn extend_data(data: &Value) -> Option<Value> {
    match data.get("extendData") {
        Some(Value::String(s)) if !s.is_empty() => Some(Value::String(s.clone())),
        Some(v) if !v.is_null() && !v.is_string() => Some(v.clone()),
        _ => None,
    }
}

fn with_extend(mut payload: Value, data: &Value) -> Value {
    if let Some(ext) = extend_data(data) {
        payload["extendData"] = ext;
        payload["subCommand"] = json!(3);
    }
    payload
}

/// `requestCall` params (`/api/voicecall/requestcall`).
#[must_use]
pub fn request_call_payload(
    callee_id: Value,
    call_id: Value,
    codec: Value,
    type_request: Value,
    imei: &str,
) -> Value {
    json!({
        "calleeId": callee_id,
        "callId": call_id,
        "codec": codec,
        "typeRequest": type_request,
        "imei": imei,
    })
}

/// `sendRequestCall` params (`/api/voicecall/request`).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn send_request_call_payload(
    callee_id: Value,
    rtcp_address: Value,
    rtp_address: Value,
    codec: Value,
    session: Value,
    call_id: Value,
    imei: &str,
    extend: Option<Value>,
) -> Value {
    let mut payload = json!({
        "calleeId": callee_id,
        "rtcpAddress": rtcp_address,
        "rtpAddress": rtp_address,
        "codec": codec,
        "session": session,
        "callId": call_id,
        "imei": imei,
    });
    if let Some(ext) = extend {
        payload["extendData"] = ext;
        payload["subCommand"] = json!(3);
    }
    payload
}

/// `sendRingRingCall` / `sendAnswerACKCall` share this shape.
#[must_use]
pub fn ring_or_ack_payload(peer_id: Value, call_id: Value, imei: &str) -> Value {
    json!({
        "callerId": peer_id,
        "callId": call_id,
        "status": 0,
        "imei": imei,
    })
}

/// `sendAnswerACKCall` uses `calleeId` instead of `callerId`.
#[must_use]
pub fn answer_ack_payload(callee_id: Value, call_id: Value, imei: &str) -> Value {
    json!({
        "calleeId": callee_id,
        "callId": call_id,
        "status": 0,
        "imei": imei,
    })
}

/// `sendAnswerCall` params (`/api/voicecall/answer`). `session` becomes `session_id`.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn answer_call_payload(
    caller_id: Value,
    call_id: Value,
    status: Value,
    codec: Value,
    rtcp_address: Value,
    rtp_address: Value,
    session: Value,
    extend: Option<Value>,
) -> Value {
    let mut payload = json!({
        "callerId": caller_id,
        "callId": call_id,
        "status": status,
        "codec": codec,
        "rtcpAddress": rtcp_address,
        "rtpAddress": rtp_address,
        "session_id": session,
    });
    if let Some(ext) = extend {
        payload["extendData"] = ext;
        payload["subCommand"] = json!(3);
    }
    payload
}

/// `sendCancelCall` params (`/api/voicecall/cancel`). Engine `toId` → HTTP `callerId`.
#[must_use]
pub fn cancel_call_payload(
    caller_id: Value,
    call_id: Value,
    call_type: Value,
    imei: &str,
) -> Value {
    json!({
        "callerId": caller_id,
        "callId": call_id,
        "callType": call_type,
        "status": 0,
        "imei": imei,
    })
}

/// `sendEndCall` params (`/api/voicecall/endcall`).
#[must_use]
pub fn end_call_payload(uid_to: Value, call_id: Value, imei: &str) -> Value {
    json!({
        "uidTo": uid_to,
        "callId": call_id,
        "status": 3,
        "imei": imei,
    })
}

/// `sendLogEndCall` params (`/api/voicecall/logendcall`).
#[must_use]
pub fn log_end_call_payload(
    call_id: Value,
    partner_id: Value,
    status: Value,
    logdata: Value,
    duration: Value,
    role: Value,
    imei: &str,
) -> Value {
    json!({
        "call_id": call_id,
        "partner_id": partner_id,
        "status": status,
        "logdata": logdata,
        "duration": duration,
        "role": role,
        "imei": imei,
    })
}

/// `sendOnOffCamMic` params (`/api/voicecall/conf`).
#[must_use]
pub fn conf_payload(
    callee_id: Value,
    call_id: Value,
    is_on_cam: Value,
    is_on_mic: Value,
    data: Value,
) -> Value {
    json!({
        "calleeId": callee_id,
        "callId": call_id,
        "isOnCam": is_on_cam,
        "isOnMic": is_on_mic,
        "data": data,
    })
}

/// Hold / unhold 1-1 (`holdreq` / `holdresp` / `unholdreq` / `unholdresp`).
#[must_use]
pub fn hold_payload(to_id: Value) -> Value {
    json!({ "toId": to_id })
}

/// Maps a `sendSignal` command to `(path, params)` as `compact1913.js` does.
///
/// Group commands forward the engine body and add `imei`, the same way 1-1
/// payloads carry it. 1-1 screen share (12064) is
/// [`share_screen_payload`]; group share (12044) is [`group_share_payload`].
#[must_use]
pub fn voicecall_from_signal(
    command: i64,
    data: &Value,
    imei: &str,
) -> Option<(&'static str, Value)> {
    match command {
        SIG_REQUEST_CALL => {
            let codec = match data.get("codec") {
                None | Some(Value::Null) => json!("[]"),
                Some(v) => v.clone(),
            };
            Some((
                "/api/voicecall/requestcall",
                request_call_payload(
                    clone_field(data, "calleeId"),
                    clone_field(data, "callId"),
                    codec,
                    clone_field(data, "type"),
                    imei,
                ),
            ))
        }
        SIG_SEND_REQUEST => Some((
            "/api/voicecall/request",
            with_extend(
                send_request_call_payload(
                    clone_field(data, "calleeId"),
                    clone_field(data, "rtcpAddress"),
                    clone_field(data, "rtpAddress"),
                    clone_field(data, "codec"),
                    clone_field(data, "session"),
                    clone_field(data, "callId"),
                    imei,
                    None,
                ),
                data,
            ),
        )),
        SIG_RING => Some((
            "/api/voicecall/ringring",
            ring_or_ack_payload(
                clone_field(data, "callerId"),
                clone_field(data, "callId"),
                imei,
            ),
        )),
        SIG_ANSWER_ACK => Some((
            "/api/voicecall/answerack",
            answer_ack_payload(
                clone_field(data, "calleeId"),
                clone_field(data, "callId"),
                imei,
            ),
        )),
        SIG_ANSWER => {
            let codec = match data.get("codec") {
                None | Some(Value::Null) | Some(Value::Bool(false)) => json!(""),
                Some(Value::String(s)) if s.is_empty() => json!(""),
                Some(v) => v.clone(),
            };
            Some((
                "/api/voicecall/answer",
                answer_call_payload(
                    clone_field(data, "callerId"),
                    clone_field(data, "callId"),
                    clone_field(data, "status"),
                    codec,
                    clone_field(data, "rtcpAddress"),
                    clone_field(data, "rtpAddress"),
                    clone_field(data, "session"),
                    extend_data(data),
                ),
            ))
        }
        SIG_CANCEL => Some((
            "/api/voicecall/cancel",
            cancel_call_payload(
                clone_field(data, "toId"),
                clone_field(data, "callId"),
                clone_field(data, "callType"),
                imei,
            ),
        )),
        SIG_LOG_END => Some((
            "/api/voicecall/logendcall",
            log_end_call_payload(
                clone_field(data, "callId"),
                clone_field(data, "partnerId"),
                clone_field(data, "status"),
                clone_field(data, "data"),
                clone_field(data, "duration"),
                clone_field(data, "role"),
                imei,
            ),
        )),
        SIG_END => Some((
            "/api/voicecall/endcall",
            end_call_payload(clone_field(data, "toId"), clone_field(data, "callId"), imei),
        )),
        SIG_HOLD_REQ => Some((
            "/api/voicecall/holdreq",
            hold_payload(clone_field(data, "toId")),
        )),
        SIG_HOLD_RESP => Some((
            "/api/voicecall/holdresp",
            hold_payload(clone_field(data, "toId")),
        )),
        SIG_UNHOLD_REQ => Some((
            "/api/voicecall/unholdreq",
            hold_payload(clone_field(data, "toId")),
        )),
        SIG_UNHOLD_RESP => Some((
            "/api/voicecall/unholdresp",
            hold_payload(clone_field(data, "toId")),
        )),
        SIG_CONF => Some((
            "/api/voicecall/conf",
            conf_payload(
                clone_field(data, "calleeId"),
                clone_field(data, "callId"),
                clone_field(data, "isOnCam"),
                clone_field(data, "isOnMic"),
                clone_field(data, "data"),
            ),
        )),
        SIG_GROUP_REQUEST_CALL => Some((
            "/api/voicecall/group/requestcall",
            group_payload(data, imei),
        )),
        SIG_GROUP_REQUEST => Some(("/api/voicecall/group/request", group_ring_payload(data))),
        SIG_GROUP_ANSWER => Some(("/api/voicecall/group/answer", group_payload(data, imei))),
        SIG_GROUP_CANCEL => Some(("/api/voicecall/group/cancel", group_payload(data, imei))),
        SIG_GROUP_END => Some(("/api/voicecall/group/endcall", group_payload(data, imei))),
        SIG_GROUP_RING => Some(("/api/voicecall/group/ringring", group_payload(data, imei))),
        SIG_GROUP_FINISH => Some(("/api/voicecall/group/finish", group_payload(data, imei))),
        SIG_GROUP_PING => Some(("/api/voicecall/group/ping", group_state_payload(data))),
        SIG_GROUP_BROADCAST => Some(("/api/voicecall/group/broadcast", group_state_payload(data))),
        SIG_GROUP_SHARE => Some((
            "/api/voicecall/group/sharescreen",
            group_share_payload(data),
        )),
        SIG_SHARE_SCREEN => Some((
            "/api/voicecall/requestsharescreen",
            share_screen_payload(data),
        )),
        _ => None,
    }
}

/// `sendOnOffSharescreen` (`/api/voicecall/requestsharescreen`).
///
/// compact3178.js encrypts exactly `{ uidTo, callId, status }`.
fn share_screen_payload(data: &Value) -> Value {
    json!({
        "uidTo": clone_field(data, "uidTo"),
        "callId": clone_field(data, "callId"),
        "status": clone_field(data, "status"),
    })
}

/// `sendRequestShareScreen` (`/api/voicecall/group/sharescreen`).
///
/// The engine frame uses camelCase `audioState` / `videoState`. The HTTP
/// function lowercases those two. `imei` stays off this body.
fn group_share_payload(data: &Value) -> Value {
    json!({
        "userId": clone_field(data, "userId"),
        "callId": clone_field(data, "callId"),
        "hostCall": clone_field(data, "hostCall"),
        "status": clone_field(data, "status"),
        "audiostate": data.get("audiostate").or_else(|| data.get("audioState")).cloned().unwrap_or(Value::Null),
        "videostate": data.get("videostate").or_else(|| data.get("videoState")).cloned().unwrap_or(Value::Null),
        "data": clone_field(data, "data"),
    })
}

/// 12097 / 12098 body, as `pingCallGroup` and `broadcastState` in compact3178.js.
///
/// The engine frame uses camelCase (`callState`, `audioState`, `videoState`).
/// The HTTP function lowercases those three and drops every other field,
/// including `imei`. Sending the camelCase names makes Zalo return 114.
fn group_state_payload(data: &Value) -> Value {
    let mut body = json!({
        "callId": clone_field(data, "callId"),
        "hostCall": clone_field(data, "hostCall"),
        "userId": clone_field(data, "userId"),
        "callstate": data.get("callstate").or_else(|| data.get("callState")).cloned().unwrap_or(Value::Null),
        "audiostate": data.get("audiostate").or_else(|| data.get("audioState")).cloned().unwrap_or(Value::Null),
        "videostate": data.get("videostate").or_else(|| data.get("videoState")).cloned().unwrap_or(Value::Null),
    });
    if let Some(params) = data.get("params").filter(|value| !value.is_null()) {
        body["params"] = params.clone();
    }
    body
}

/// 12434 body, as `sendRequestCallGroup` in compact3178.js.
///
/// The encrypted object is exactly these fields. `imei` stays on the query
/// string; putting it inside this body made the ring return status 2.
fn group_ring_payload(data: &Value) -> Value {
    let mut body = serde_json::Map::new();
    for key in [
        "calleeId", "callId", "callType", "data", "session", "partners", "groupId",
    ] {
        if let Some(value) = data.get(key).filter(|value| !value.is_null()) {
            body.insert(key.to_owned(), value.clone());
        }
    }
    Value::Object(body)
}

/// Group HTTP body: the engine's fields, plus `imei` when it is missing.
fn group_payload(data: &Value, imei: &str) -> Value {
    let mut payload = match data.clone() {
        Value::Object(map) => Value::Object(map),
        _ => json!({}),
    };
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("imei".to_owned())
            .or_insert_with(|| Value::String(imei.to_owned()));
    }
    payload
}

/// `sendSignal` command number and `data` from an engine frame.
#[must_use]
pub fn send_signal_parts(frame: &Value) -> Option<(i64, Value)> {
    if frame.get("type").and_then(Value::as_str) != Some("sendSignal") {
        return None;
    }
    let command = frame.get("command").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_u64().map(|n| n as i64))
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })?;
    let data = frame.get("data").cloned().unwrap_or(Value::Null);
    Some((command, data))
}

/// Reads a `sendSignal` engine frame and maps it like [`voicecall_from_signal`].
#[must_use]
pub fn voicecall_from_frame(frame: &Value, imei: &str) -> Option<(&'static str, Value)> {
    let (command, data) = send_signal_parts(frame)?;
    voicecall_from_signal(command, &data, imei)
}

/// compact3545 `kCOK`: after decrypt, the HTTP promise resolves with `r.data`.
///
/// [`ZaloApi::get_encrypted`] returns the decrypted envelope
/// `{ error_code, data: {…} }`. Native `handleRecvSignal` gets the inner
/// object (`servers`, `sessId`, `fromId`, `toId`, `zrtc_config` on 401).
#[must_use]
pub fn voicecall_http_payload(http_json: &Value) -> &Value {
    match http_json.get("data") {
        Some(v) if !v.is_null() => v,
        _ => http_json,
    }
}

/// compact1913 `handleRecvSignal`: `{ type: "recvSignal", command, data }`.
#[must_use]
pub fn recv_signal_from_http(command: i64, http_json: &Value) -> Value {
    json!({
        "type": "recvSignal",
        "command": command,
        "data": voicecall_http_payload(http_json),
    })
}

/// compact1913 `handleControl`: `{ type: "control", data: voipEvent }`.
#[must_use]
pub fn control_frame(ctrl: &Value) -> Value {
    json!({ "type": "control", "data": ctrl })
}

/// Incoming 1-1 call parsed from a voip control (`act: "request"`).
#[derive(Clone, Debug, PartialEq)]
pub struct IncomingCall {
    /// `uidN` of the caller (Zalo id the GUI uses as the thread id).
    pub uid: String,
    /// Display name from `params.Dname`. Empty when the control omits it.
    pub name: String,
    /// `true` when `extendData.callType == 1` (video).
    pub video: bool,
    /// The voip control event to feed EngineCore as `{ type: "control", data }`.
    pub control: Value,
}

fn parse_maybe_json(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
        Some(other) => other.clone(),
        None => json!({}),
    }
}

fn json_text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// compact3043 `_processCtrlMessages` case `"voip"`: events with `act_type: voip`.
///
/// Live calls arrive on `cmd 601` (`CTRL_PUSH_CTRL`); catch-up on `cmd 603`.
/// `data` may still be a JSON string, as the bundle parses it before the
/// `callbackVideoCallEvent` array.
#[must_use]
pub fn voip_controls_from_event(event: &Value) -> Vec<Value> {
    let root = event.get("data").filter(|v| v.is_object()).unwrap_or(event);
    let Some(list) = root
        .get("controls")
        .and_then(Value::as_array)
        .or_else(|| root.get("actions").and_then(Value::as_array))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in list {
        let mut ctrl = item
            .get("content")
            .filter(|v| v.is_object())
            .unwrap_or(item)
            .clone();
        if ctrl.get("act_type").and_then(Value::as_str) != Some("voip") {
            continue;
        }
        if let Some(Value::String(text)) = ctrl.get("data").cloned() {
            let trimmed = text.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    ctrl["data"] = parsed;
                }
            }
        }
        out.push(ctrl);
    }
    out
}

/// compact1913 drops control events whose `data.ts` is older than 60 s.
#[must_use]
pub fn voip_control_is_fresh(ctrl: &Value, now_ms: i64) -> bool {
    let ts = ctrl
        .get("data")
        .and_then(|d| d.get("ts"))
        .or_else(|| ctrl.get("ts"));
    let Some(ts) = ts else {
        return true;
    };
    let n = ts
        .as_i64()
        .or_else(|| ts.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    if n == 0 {
        return true;
    }
    now_ms.saturating_sub(n) <= 60_000
}

/// compact1913 `isIncomingCallEvent`: `act == "request"` (1-1). Group stays
/// out of this slice.
#[must_use]
pub fn incoming_call_from_control(ctrl: &Value) -> Option<IncomingCall> {
    if ctrl.get("act").and_then(Value::as_str) != Some("request") {
        return None;
    }
    let d = ctrl.get("data").cloned().unwrap_or(json!({}));
    let uid = json_text(&d, "uidN");
    if uid.is_empty() {
        return None;
    }
    let params = parse_maybe_json(d.get("params"));
    let name = json_text(&params, "Dname");
    let ext = parse_maybe_json(params.get("extendData"));
    let call_type = ext.get("callType").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });
    let video = call_type == Some(1);
    Some(IncomingCall {
        uid,
        name,
        video,
        control: ctrl.clone(),
    })
}

impl ZaloApi {
    fn voice_call_base(&self) -> String {
        self.service_base("voice_call", VOICE_CALL_FALLBACK)
            .to_owned()
    }

    /// GET one `/api/voicecall/*` path with encrypted params.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn voicecall_get(&self, path: &str, payload: &Value) -> Result<Value, ClientError> {
        self.get_encrypted(&self.voice_call_base(), path, payload)
            .await
    }

    /// Translator used by the GUI: `sendSignal` command → HTTP, or `Ok(None)`
    /// when the command is group/video-share and out of this slice.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_voicecall_signal(
        &self,
        command: i64,
        data: &Value,
    ) -> Result<Option<Value>, ClientError> {
        let Some((path, payload)) = voicecall_from_signal(command, data, &self.device.imei) else {
            return Ok(None);
        };
        Ok(Some(self.voicecall_get(path, &payload).await?))
    }

    /// `requestCall` (`/api/voicecall/requestcall`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn request_call(
        &self,
        callee_id: &str,
        call_id: i64,
        codec: &str,
        type_request: i64,
    ) -> Result<Value, ClientError> {
        let payload = request_call_payload(
            json!(callee_id),
            json!(call_id),
            json!(codec),
            json!(type_request),
            &self.device.imei,
        );
        self.voicecall_get("/api/voicecall/requestcall", &payload)
            .await
    }

    /// `sendRequestCall` (`/api/voicecall/request`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_request_call(&self, data: &Value) -> Result<Value, ClientError> {
        let (_, payload) = voicecall_from_signal(SIG_SEND_REQUEST, data, &self.device.imei)
            .expect("416 always maps");
        self.voicecall_get("/api/voicecall/request", &payload).await
    }

    /// `sendRingRingCall` (`/api/voicecall/ringring`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_ringring_call(
        &self,
        caller_id: &str,
        call_id: i64,
    ) -> Result<Value, ClientError> {
        let payload = ring_or_ack_payload(json!(caller_id), json!(call_id), &self.device.imei);
        self.voicecall_get("/api/voicecall/ringring", &payload)
            .await
    }

    /// `sendAnswerCall` (`/api/voicecall/answer`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_answer_call(&self, data: &Value) -> Result<Value, ClientError> {
        let (_, payload) =
            voicecall_from_signal(SIG_ANSWER, data, &self.device.imei).expect("402 always maps");
        self.voicecall_get("/api/voicecall/answer", &payload).await
    }

    /// `sendAnswerACKCall` (`/api/voicecall/answerack`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_answer_ack_call(
        &self,
        callee_id: &str,
        call_id: i64,
    ) -> Result<Value, ClientError> {
        let payload = answer_ack_payload(json!(callee_id), json!(call_id), &self.device.imei);
        self.voicecall_get("/api/voicecall/answerack", &payload)
            .await
    }

    /// `sendCancelCall` (`/api/voicecall/cancel`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_cancel_call(
        &self,
        caller_id: &str,
        call_id: i64,
        call_type: i64,
    ) -> Result<Value, ClientError> {
        let payload = cancel_call_payload(
            json!(caller_id),
            json!(call_id),
            json!(call_type),
            &self.device.imei,
        );
        self.voicecall_get("/api/voicecall/cancel", &payload).await
    }

    /// `sendEndCall` (`/api/voicecall/endcall`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_end_call(&self, uid_to: &str, call_id: i64) -> Result<Value, ClientError> {
        let payload = end_call_payload(json!(uid_to), json!(call_id), &self.device.imei);
        self.voicecall_get("/api/voicecall/endcall", &payload).await
    }

    /// `sendLogEndCall` (`/api/voicecall/logendcall`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_log_end_call(&self, data: &Value) -> Result<Value, ClientError> {
        let (_, payload) =
            voicecall_from_signal(SIG_LOG_END, data, &self.device.imei).expect("406 always maps");
        self.voicecall_get("/api/voicecall/logendcall", &payload)
            .await
    }

    /// `sendOnOffCamMic` (`/api/voicecall/conf`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_on_off_cam_mic(&self, data: &Value) -> Result<Value, ClientError> {
        let (_, payload) =
            voicecall_from_signal(SIG_CONF, data, &self.device.imei).expect("12006 always maps");
        self.voicecall_get("/api/voicecall/conf", &payload).await
    }

    /// `sendHoldRequestCall` (`/api/voicecall/holdreq`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_hold_request(&self, to_id: &str) -> Result<Value, ClientError> {
        self.voicecall_get("/api/voicecall/holdreq", &hold_payload(json!(to_id)))
            .await
    }

    /// `sendHoldResponseCall` (`/api/voicecall/holdresp`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_hold_response(&self, to_id: &str) -> Result<Value, ClientError> {
        self.voicecall_get("/api/voicecall/holdresp", &hold_payload(json!(to_id)))
            .await
    }

    /// `sendUnHoldRequestCall` (`/api/voicecall/unholdreq`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_unhold_request(&self, to_id: &str) -> Result<Value, ClientError> {
        self.voicecall_get("/api/voicecall/unholdreq", &hold_payload(json!(to_id)))
            .await
    }

    /// `sendUnHoldResponseCall` (`/api/voicecall/unholdresp`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_unhold_response(&self, to_id: &str) -> Result<Value, ClientError> {
        self.voicecall_get("/api/voicecall/unholdresp", &hold_payload(json!(to_id)))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_call_payload_matches_bundle() {
        let p = request_call_payload(
            json!("222"),
            json!(1_834_567_890),
            json!("[]\n"),
            json!(1),
            "imei",
        );
        assert_eq!(p["calleeId"], "222");
        assert_eq!(p["callId"], 1_834_567_890);
        assert_eq!(p["codec"], "[]\n");
        assert_eq!(p["typeRequest"], 1);
        assert_eq!(p["imei"], "imei");
        assert_eq!(p.get("subCommand"), None);
    }

    #[test]
    fn send_signal_401_416_405_map_like_compact1913() {
        let imei = "imei";
        let (path, p) = voicecall_from_signal(
            SIG_REQUEST_CALL,
            &json!({"calleeId": "222", "callId": 9, "codec": "[]\n", "type": 3}),
            imei,
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/requestcall");
        assert_eq!(p["typeRequest"], 3);
        assert_eq!(p["codec"], "[]\n");

        let (path, p) = voicecall_from_signal(
            SIG_SEND_REQUEST,
            &json!({
                "calleeId": "222",
                "rtcpAddress": "1.1.1.1|8000",
                "rtpAddress": "1.1.1.1|8001",
                "codec": "opus/16000/1",
                "session": "sess",
                "callId": 9,
                "extendData": "{\"p2p\":[]}\n",
            }),
            imei,
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/request");
        assert_eq!(p["session"], "sess");
        assert_eq!(p["subCommand"], 3);
        assert!(p["extendData"].as_str().unwrap().contains("p2p"));

        let (path, p) = voicecall_from_signal(
            SIG_CANCEL,
            &json!({"toId": "222", "callId": 9, "callType": 0, "status": 0}),
            imei,
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/cancel");
        assert_eq!(p["callerId"], "222");
        assert_eq!(p["status"], 0);
    }

    #[test]
    fn answer_end_log_conf_and_hold_map() {
        let imei = "imei";
        let (path, p) = voicecall_from_signal(
            SIG_ANSWER,
            &json!({
                "callerId": "111",
                "callId": 9,
                "status": 0,
                "codec": "opus",
                "rtcpAddress": "a",
                "rtpAddress": "b",
                "session": "sess",
                "extendData": "{}\n",
            }),
            imei,
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/answer");
        assert_eq!(p["session_id"], "sess");
        assert_eq!(p.get("session"), None);
        assert_eq!(p.get("imei"), None);
        assert_eq!(p["subCommand"], 3);

        let (path, p) =
            voicecall_from_signal(SIG_END, &json!({"toId": "222", "callId": 9}), imei).unwrap();
        assert_eq!(path, "/api/voicecall/endcall");
        assert_eq!(p["uidTo"], "222");
        assert_eq!(p["status"], 3);

        let (path, p) = voicecall_from_signal(
            SIG_LOG_END,
            &json!({
                "callId": 9,
                "partnerId": "222",
                "status": 50,
                "data": "{\"NewZRTC\":1}",
                "duration": 12,
                "role": true,
            }),
            imei,
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/logendcall");
        assert_eq!(p["call_id"], 9);
        assert_eq!(p["partner_id"], "222");
        assert_eq!(p["logdata"], "{\"NewZRTC\":1}");
        assert_eq!(p["role"], true);

        let (path, p) = voicecall_from_signal(
            SIG_CONF,
            &json!({"calleeId": "222", "callId": 9, "isOnCam": 1, "isOnMic": -1, "data": ""}),
            imei,
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/conf");
        assert_eq!(p["isOnCam"], 1);
        assert_eq!(p.get("imei"), None);

        assert_eq!(
            voicecall_from_signal(SIG_HOLD_REQ, &json!({"toId": "222"}), imei)
                .unwrap()
                .0,
            "/api/voicecall/holdreq"
        );
        assert_eq!(
            voicecall_from_signal(SIG_HOLD_RESP, &json!({"toId": "222"}), imei)
                .unwrap()
                .0,
            "/api/voicecall/holdresp"
        );
        assert_eq!(
            voicecall_from_signal(SIG_UNHOLD_REQ, &json!({"toId": "222"}), imei)
                .unwrap()
                .0,
            "/api/voicecall/unholdreq"
        );
        assert_eq!(
            voicecall_from_signal(SIG_UNHOLD_RESP, &json!({"toId": "222"}), imei)
                .unwrap()
                .0,
            "/api/voicecall/unholdresp"
        );
        assert_eq!(
            voicecall_from_signal(SIG_RING, &json!({"callerId": "111", "callId": 9}), imei)
                .unwrap()
                .0,
            "/api/voicecall/ringring"
        );
        assert_eq!(
            voicecall_from_signal(
                SIG_ANSWER_ACK,
                &json!({"calleeId": "222", "callId": 9}),
                imei
            )
            .unwrap()
            .0,
            "/api/voicecall/answerack"
        );
    }

    #[test]
    fn group_signals_map_to_group_paths() {
        let (path, payload) = voicecall_from_signal(
            12433,
            &json!({"callId": 9, "groupId": "42", "partners": "[]"}),
            "imei",
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/group/requestcall");
        assert_eq!(payload["callId"], 9);
        assert_eq!(payload["imei"], "imei");
        let (path, body) = voicecall_from_signal(
            12434,
            &json!({
                "calleeId": "111",
                "callId": 9,
                "callType": 1,
                "data": "payload",
                "session": "sess",
                "partners": "[\"111\"]\n",
                "groupId": "42",
                "imei": "should-not-be-copied"
            }),
            "imei",
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/group/request");
        assert_eq!(body["calleeId"], "111");
        assert_eq!(body["callId"], 9);
        assert_eq!(body["callType"], 1);
        assert_eq!(body["groupId"], "42");
        assert!(body.get("imei").is_none());
        assert_eq!(
            voicecall_from_signal(12097, &json!({}), "imei").unwrap().0,
            "/api/voicecall/group/ping"
        );
        let (path, body) = voicecall_from_signal(
            12098,
            &json!({
                "audioState": 0,
                "callId": 1_834_567_890_i64,
                "callState": 3,
                "hostCall": 42,
                "userId": 7,
                "videoState": 0,
                "imei": "should-not-be-copied"
            }),
            "imei",
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/group/broadcast");
        assert_eq!(body["callId"], 1_834_567_890_i64);
        assert_eq!(body["callstate"], 3);
        assert_eq!(body["audiostate"], 0);
        assert_eq!(body["videostate"], 0);
        assert!(body.get("callState").is_none());
        assert!(body.get("audioState").is_none());
        assert!(body.get("imei").is_none());
        assert!(body.get("params").is_none());
        let (path, body) = voicecall_from_signal(
            12064,
            &json!({"uidTo": "9", "callId": 9, "status": 1}),
            "imei",
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/requestsharescreen");
        assert_eq!(body["uidTo"], "9");
        assert_eq!(body["status"], 1);
        assert!(body.get("imei").is_none());
        let (path, body) = voicecall_from_signal(
            12044,
            &json!({
                "userId": 7,
                "callId": 9,
                "hostCall": 42,
                "status": 1,
                "audioState": 0,
                "videoState": 1,
                "data": "{\"shareScreenId\":0}",
                "imei": "should-not-be-copied"
            }),
            "imei",
        )
        .unwrap();
        assert_eq!(path, "/api/voicecall/group/sharescreen");
        assert_eq!(body["audiostate"], 0);
        assert_eq!(body["videostate"], 1);
        assert_eq!(body["data"], "{\"shareScreenId\":0}");
        assert!(body.get("audioState").is_none());
        assert!(body.get("imei").is_none());
        let frame = json!({
            "type": "sendSignal",
            "command": 401,
            "data": {"calleeId": "222", "callId": 9, "type": 1}
        });
        let (path, p) = voicecall_from_frame(&frame, "imei").unwrap();
        assert_eq!(path, "/api/voicecall/requestcall");
        assert_eq!(p["codec"], "[]");
        assert_eq!(p["typeRequest"], 1);
        assert!(voicecall_from_frame(&json!({"type": "update", "command": 401}), "imei").is_none());
    }

    #[test]
    fn engine_401_frame_roundtrip() {
        let frame = json!({
            "type": "sendSignal",
            "command": 401,
            "data": {
                "callId": 1_834_567_890_i64,
                "calleeId": "10009999",
                "codec": "[]\n",
                "type": 1
            }
        });
        let (path, p) = voicecall_from_frame(&frame, "imei-test").unwrap();
        assert_eq!(path, "/api/voicecall/requestcall");
        assert_eq!(p["calleeId"], "10009999");
        assert!(p["callId"].as_i64().unwrap() < 2_147_483_648);
        assert!(p["callId"].as_i64().unwrap() >= 1_000_000_000);
        assert_eq!(VOICE_CALL_FALLBACK, "https://tt-voicecall-wpa.chat.zalo.me");
    }

    #[test]
    fn recv_signal_from_http_unwraps_k_cok_data() {
        let envelope = json!({
            "error_code": 0,
            "error_message": "Successful.",
            "data": {
                "servers": [{"rtpaddr": "1.2.3.4|5000", "rtcpaddr": "1.2.3.4|5001"}],
                "sessId": "sess-401",
                "fromId": 111,
                "toId": 222,
                "zrtc_config": {"srtpMode": 1}
            }
        });
        let frame = recv_signal_from_http(SIG_REQUEST_CALL, &envelope);
        assert_eq!(frame["type"], "recvSignal");
        assert_eq!(frame["command"], 401);
        assert_eq!(frame["data"]["sessId"], "sess-401");
        assert_eq!(frame["data"]["servers"][0]["rtpaddr"], "1.2.3.4|5000");
        assert!(frame["data"].get("error_code").is_none());

        let flat = json!({
            "servers": [{"rtpaddr": "9.9.9.9|1"}],
            "sessId": "already-unwrapped"
        });
        let frame = recv_signal_from_http(401, &flat);
        assert_eq!(frame["data"]["sessId"], "already-unwrapped");
    }

    #[test]
    fn voip_controls_from_601_match_compact3043() {
        let params = serde_json::to_string(&json!({
            "Dname": "Peer",
            "extendData": serde_json::to_string(&json!({"callType": 1})).unwrap(),
        }))
        .unwrap();
        let data_text = serde_json::to_string(&json!({
            "uidN": "10009999",
            "callId": 1_834_567_890_i64,
            "session": "sess",
            "rtpAddress": "1.1.1.1|1",
            "rtcpAddress": "1.1.1.1|2",
            "params": params,
        }))
        .unwrap();
        let event = json!({
            "controls": [{
                "controlId": "1",
                "content": {
                    "act_type": "voip",
                    "act": "request",
                    "data": data_text,
                }
            }, {
                "content": {"act_type": "transfer_sync2", "act": "upload_batch", "data": {}}
            }]
        });
        let voip = voip_controls_from_event(&event);
        assert_eq!(voip.len(), 1);
        assert_eq!(voip[0]["act"], "request");
        let incoming = incoming_call_from_control(&voip[0]).expect("request");
        assert_eq!(incoming.uid, "10009999");
        assert_eq!(incoming.name, "Peer");
        assert!(incoming.video);
        assert_eq!(control_frame(&voip[0])["type"], "control");

        let nested = json!({"data": {"controls": [{
            "act_type": "voip",
            "act": "end_call",
            "data": {"callId": 9, "ts": 1}
        }]}});
        let voip = voip_controls_from_event(&nested);
        assert_eq!(voip.len(), 1);
        assert!(incoming_call_from_control(&voip[0]).is_none());
        assert!(voip_control_is_fresh(&voip[0], 50_000));
        assert!(!voip_control_is_fresh(&voip[0], 70_000));
    }
}
