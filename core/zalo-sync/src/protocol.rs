//! Sync 2 wire protocol: socket commands `590`/`591`/`592`, enums (`SCpg`),
//! JSON payload builders, `601`/`603` control parser (`gne`..`One`), and `Wte` retry rules.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// `SYNC_MESSAGE.REQUEST` (`compact2101.js`).
pub const CMD_SYNC_REQUEST: u16 = 590;
/// `SYNC_MESSAGE.ACK_DELETE_SYNC_SESSION` (`compact2101.js`).
pub const CMD_SYNC_ACK_DELETE: u16 = 591;
/// `SYNC_MESSAGE.REQUEST_MOBILE_WAKE_UP` (`compact2101.js`).
pub const CMD_SYNC_WAKEUP: u16 = 592;

/// Default socket timeout for `590` (`compact3857.js`: `10000` ms).
pub const TIMEOUT_SYNC_REQUEST_MS: u64 = 10_000;
/// Default socket timeout for `591` and `592` (`compact3857.js`: `3000` ms).
pub const TIMEOUT_SYNC_CONTROL_MS: u64 = 3_000;
/// Maximum age of a `transfer_after_login` tempKey (`main-startup.js:119843`: `600_000` ms).
pub const TRANSFER_AFTER_LOGIN_MAX_AGE_MS: u64 = 600_000;

/// Wire `syncType` (`SCpg` in `compact2186.js`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SyncType {
    /// `SyncFull = 0`
    SyncFull = 0,
    /// `SyncOnDemand = 1`
    SyncOnDemand = 1,
}

/// Wire `toDevice` / `fromDevice` (`SCpg` in `compact2186.js`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum DeviceType {
    /// `Mobile = 0`
    Mobile = 0,
    /// `Web = 1`
    Web = 1,
    /// `PC = 2`
    Pc = 2,
}

/// Phone status carried in `transfer_status.data.status` (`SCpg`, `fromDevice == 0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MobileStatus {
    /// `Active = 1`
    Active = 1,
    /// `Idle = 2`
    Idle = 2,
    /// `WaitingConfirm = 3`
    WaitingConfirm = 3,
    /// `Confirmed = 4`
    Confirmed = 4,
    /// `BypassConfirmed = 5`
    BypassConfirmed = 5,
    /// `UserReject = 6`
    UserReject = 6,
    /// `UserCancel = 7` (NOTE: distinct from `DisposeReason::UserCancel = 0`!)
    UserCancel = 7,
    /// `MasterDeviceBusy = 8`
    MasterDeviceBusy = 8,
    /// `MasterDeviceLowStorage = 9`
    MasterDeviceLowStorage = 9,
    /// `MasterDeviceLogout = 10`
    MasterDeviceLogout = 10,
}

impl MobileStatus {
    /// Parses a wire integer (1..=10) into [`MobileStatus`].
    #[must_use]
    pub const fn from_u64(v: u64) -> Option<Self> {
        match v {
            1 => Some(Self::Active),
            2 => Some(Self::Idle),
            3 => Some(Self::WaitingConfirm),
            4 => Some(Self::Confirmed),
            5 => Some(Self::BypassConfirmed),
            6 => Some(Self::UserReject),
            7 => Some(Self::UserCancel),
            8 => Some(Self::MasterDeviceBusy),
            9 => Some(Self::MasterDeviceLowStorage),
            10 => Some(Self::MasterDeviceLogout),
            _ => None,
        }
    }
}

/// Wire `reason` for command `591` (`ACK_DELETE_SYNC_SESSION`, `SCpg`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum DisposeReason {
    /// `UserCancel = 0` (never send `7` here!).
    UserCancel = 0,
    /// `SyncSuccess = 1`
    SyncSuccess = 1,
    /// `Error = 2`
    Error = 2,
    /// `OutDated = 3`
    OutDated = 3,
}

/// Wire `batchType` (`PL82` in `compact1981.js`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum BatchType {
    /// `Message = 1`
    Message = 1,
    /// `MetadataEmbedded = 2`
    MetadataEmbedded = 2,
}

impl BatchType {
    /// Parses a wire integer (`1` or `2`).
    #[must_use]
    pub const fn from_u64(v: u64) -> Option<Self> {
        match v {
            1 => Some(Self::Message),
            2 => Some(Self::MetadataEmbedded),
            _ => None,
        }
    }
}

/// Wire `req.priority` (`vuBx` in `compact4376.js`).
///
/// Renderer copies these exact values into socket `590`:
/// - Conversations list & preview: `High` (`0`)
/// - Prepare remaining & other messages: `Low` (`2`)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum SyncPriority {
    /// `HIGH = 0`
    High = 0,
    /// `NORMAL = 1`
    Normal = 1,
    /// `LOW = 2`
    Low = 2,
}

/// Wire `req.type` (`databaseName` on `RemoteDatabase` in `shared-worker` lines 7405 & 8151/8154).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncDatabaseKind {
    /// `"conversation"` (`createConversationDatabase`)
    Conversation,
    /// `"message"` (`createMessageDatabase`)
    Message,
}

impl SyncDatabaseKind {
    /// Wire string placed in `req.type`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Message => "message",
        }
    }
}

/// One wire query entry inside `req.queries[]` (`shared-worker` line 7403).
/// Only `partition`, `from`, `to`, `limit` go on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireQuery {
    /// `""` for conversation list; `"oneone/{globalId}"` or `"group/{globalId}"` for messages.
    pub partition: String,
    /// Inclusive lower bound (ms).
    pub from: u64,
    /// Inclusive upper bound (ms).
    pub to: u64,
    /// Maximum items requested.
    pub limit: u64,
}

/// Parameters for building a socket `590` (`REQUEST`) JSON body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncRequest590 {
    /// Random 32-char alphanumeric session id.
    pub sync_id: String,
    /// `SyncType::SyncFull` (`0`) for full sync.
    pub sync_type: SyncType,
    /// Raw ephemeral public key bytes from `libzproto_wasm_bg`.
    pub ek_public: Vec<u8>,
    /// Raw identity public key bytes from `libzproto_wasm_bg`.
    pub ik_public: Vec<u8>,
    /// Target device (`DeviceType::Mobile = 0`).
    pub to_device: DeviceType,
    /// One-time tempKey (`""` for manual sync or for any session after the first).
    pub temp_key: String,
    /// Local device hostname.
    pub device_name: String,
    /// `"conversation"` or `"message"`.
    pub db_kind: SyncDatabaseKind,
    /// Wire priority (`0` = High, `2` = Low).
    pub priority: SyncPriority,
    /// Queries on the wire (`1` for GetAll, `1..=29` for PrepareAll).
    pub queries: Vec<WireQuery>,
    /// `transferBatchSize` (fallback `2000`).
    pub batch_size: u64,
    /// Per-`userSyncSessionId` monotonic 0-based index (`ussidx`).
    pub ussidx: u64,
}

impl SyncRequest590 {
    /// Builds the exact `data` body sent inside `Frame::json(590, 0, &body)`
    /// (`compact3857.js`: `{ reqId, data: { syncId, syncType, ek, ik, ... } }`).
    #[must_use]
    pub fn to_frame_body(&self, req_id: &str) -> Value {
        let queries: Vec<Value> = self
            .queries
            .iter()
            .map(|q| {
                json!({
                    "partition": q.partition,
                    "from": q.from,
                    "to": q.to,
                    "limit": q.limit,
                })
            })
            .collect();
        json!({
            "reqId": req_id,
            "data": {
                "syncId": self.sync_id,
                "syncType": self.sync_type as u8,
                "ek": B64.encode(&self.ek_public),
                "ik": B64.encode(&self.ik_public),
                "toDevice": self.to_device as u8,
                "tempKey": self.temp_key,
                "deviceName": self.device_name,
                "req": {
                    "type": self.db_kind.as_str(),
                    "priority": self.priority as u8,
                    "queries": queries,
                    "batchSize": self.batch_size,
                },
                "ver": 1,
                "ussidx": self.ussidx,
            }
        })
    }
}

/// Builds the exact `data` body for socket `591` (`ACK_DELETE_SYNC_SESSION`):
/// `{ reqId, data: { syncId, toDevice, reason } }` (`compact3857.js:230`).
#[must_use]
pub fn build_591_body(
    req_id: &str,
    sync_id: &str,
    to_device: DeviceType,
    reason: DisposeReason,
) -> Value {
    json!({
        "reqId": req_id,
        "data": {
            "syncId": sync_id,
            "toDevice": to_device as u8,
            "reason": reason as u8,
        }
    })
}

/// Builds the exact `data` body for socket `592` (`REQUEST_MOBILE_WAKE_UP`):
/// `{ reqId, syncId, toDevice }` — NOTE: NOT wrapped in an inner `data` key (`compact3857.js:224`)!
#[must_use]
pub fn build_592_body(req_id: &str, sync_id: &str, to_device: DeviceType) -> Value {
    json!({
        "reqId": req_id,
        "syncId": sync_id,
        "toDevice": to_device as u8,
    })
}

/// Formats a message partition string (`Pc` in `shared-worker` line 8292):
/// `group/{global_noised_id}` or `oneone/{global_noised_id}`.
#[must_use]
pub fn format_partition(global_noised_id: &str, is_group: bool) -> String {
    if is_group {
        format!("group/{global_noised_id}")
    } else {
        format!("oneone/{global_noised_id}")
    }
}

/// Generates a 32-character random alphanumeric string (`Ot.i.generateRandomString(32)`).
#[must_use]
pub fn generate_sync_id() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// One scope descriptor carried inside an `upload_batch` / `transfer_error` control event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchScope {
    /// Partition string (`""` for conversations, `"oneone/..."` or `"group/..."` for messages).
    pub partition: String,
    /// Lower timestamp bound (`0` when absent).
    pub from: u64,
    /// Upper timestamp bound (`JS_MAX_SAFE_INTEGER` when absent).
    pub to: u64,
}

/// Decoded `upload_batch` or `transfer_error` payload passed into `SyncSession::handle_response`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadBatchPayload {
    /// Matching session `syncId`.
    pub sync_id: String,
    /// 0-based batch index.
    pub idx: u64,
    /// True if this is the final batch (`totalBatch = idx + 1`).
    pub is_last: bool,
    /// `0` for success; non-zero on error (`errorCode` for `transfer_error`).
    pub err: i64,
    /// `BatchType::Message` (`1`) or `BatchType::MetadataEmbedded` (`2`).
    pub batch_type: BatchType,
    /// HTTPS URL of the encrypted 4-byte-LE-framed chunk file.
    pub msg_url: String,
    /// Scopes covered by this batch.
    pub scopes: Vec<BatchScope>,
}

/// Decoded `transfer_status` payload (`fromDevice == Mobile`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MobileStatusPayload {
    /// Session id.
    pub sync_id: String,
    /// Phone status (`1..=10`).
    pub status: MobileStatus,
}

/// Decoded `transfer_after_login` auto-sync tempKey event (validated by predicate `gne`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferAfterLoginEvent {
    /// One-time tempKey to pass into `startAutoSyncFull`.
    pub temp_key: String,
    /// Target client IMEI/UUID.
    pub imei: String,
    /// Event timestamp (ms).
    pub ts: u64,
}

/// Parsed Sync 2 control events extracted from a `601`/`603` WebSocket frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncControlEvent {
    /// Valid `transfer_after_login` (`act_type == "transfer_msg"`, matching IMEI, age ≤ 600s).
    TransferAfterLogin(TransferAfterLoginEvent),
    /// `transfer_status` from `Mobile` (`fromDevice == 0`, status 1..=10).
    MobileStatus(MobileStatusPayload),
    /// `upload_batch` with `err == 0`.
    UploadBatch(UploadBatchPayload),
    /// `upload_batch` with `err != 0`, or `transfer_error`.
    BatchError(UploadBatchPayload),
}

/// Parses Sync 2 control events from a decoded `601`/`603` WebSocket event JSON,
/// implementing Electron's exact predicates `gne`..`One` (`main-startup.js:119601-119850`).
///
/// - Only `act_type == "transfer_sync2"` and `act_type == "transfer_msg"` are inspected.
/// - `transfer_after_login` is accepted ONLY when `act_type == "transfer_msg"`, `temp_key`
///   is non-empty, `data.imei == client_id`, and `now_ms.saturating_sub(ts) <= 600_000`.
///   When multiple valid `transfer_after_login` events appear in one batch, only the latest
///   (highest `ts`) is emitted (`getLatestValidTransferAfterLoginEvent`).
#[must_use]
pub fn parse_sync_controls(
    event: &Value,
    client_id: &str,
    now_ms: u64,
) -> Vec<SyncControlEvent> {
    let root = event.get("data").filter(|v| v.is_object()).unwrap_or(event);
    let Some(list) = root.get("controls").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut latest_login: Option<TransferAfterLoginEvent> = None;

    for item in list {
        let ctrl = item
            .get("content")
            .filter(|v| v.is_object())
            .unwrap_or(item);
        let Some(act_type) = ctrl.get("act_type").and_then(Value::as_str) else {
            continue;
        };
        if act_type != "transfer_sync2" && act_type != "transfer_msg" {
            continue;
        }
        let Some(act) = ctrl.get("act").and_then(Value::as_str) else {
            continue;
        };
        let data = match ctrl.get("data") {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
            Some(v) => v.clone(),
            None => Value::Null,
        };
        if !data.is_object() {
            continue;
        }

        match act {
            "transfer_after_login" => {
                // Predicate `gne`: MUST be act_type == "transfer_msg", non-empty temp_key, matching imei.
                if act_type != "transfer_msg" {
                    continue;
                }
                let temp_key = data
                    .get("temp_key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let imei = data
                    .get("imei")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let ts = json_u64(ctrl.get("ts"))
                    .or_else(|| json_u64(data.get("ts")))
                    .unwrap_or(0);
                if temp_key.is_empty() || imei != client_id {
                    continue;
                }
                if now_ms.saturating_sub(ts) <= TRANSFER_AFTER_LOGIN_MAX_AGE_MS {
                    let replace = latest_login.as_ref().is_none_or(|prev| ts >= prev.ts);
                    if replace {
                        latest_login = Some(TransferAfterLoginEvent { temp_key, imei, ts });
                    }
                }
            }
            "transfer_status" => {
                let from_device = json_u64(data.get("fromDevice"));
                if from_device != Some(DeviceType::Mobile as u64) {
                    continue;
                }
                let Some(status) = json_u64(data.get("status")).and_then(MobileStatus::from_u64)
                else {
                    continue;
                };
                let sync_id = data
                    .get("syncId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                out.push(SyncControlEvent::MobileStatus(MobileStatusPayload {
                    sync_id,
                    status,
                }));
            }
            "upload_batch" => {
                let err = json_i64(data.get("err")).unwrap_or(0);
                if let Some(payload) = parse_batch_payload(&data, err) {
                    if err == 0 {
                        out.push(SyncControlEvent::UploadBatch(payload));
                    } else {
                        out.push(SyncControlEvent::BatchError(payload));
                    }
                }
            }
            "transfer_error" => {
                let err = json_i64(data.get("errorCode"))
                    .or_else(|| json_i64(data.get("err")))
                    .unwrap_or(-1);
                if let Some(payload) = parse_batch_payload(&data, err) {
                    out.push(SyncControlEvent::BatchError(payload));
                }
            }
            _ => {}
        }
    }

    if let Some(ev) = latest_login {
        out.insert(0, SyncControlEvent::TransferAfterLogin(ev));
    }
    out
}

fn parse_batch_payload(data: &Value, err: i64) -> Option<UploadBatchPayload> {
    let batch_type = json_u64(data.get("batchType")).and_then(BatchType::from_u64)?;
    let sync_id = data.get("syncId").and_then(Value::as_str)?.to_owned();
    let idx = json_u64(data.get("idx")).unwrap_or(0);
    let is_last = data
        .get("isLast")
        .and_then(|v| {
            v.as_bool()
                .or_else(|| v.as_i64().map(|n| n != 0))
        })
        .unwrap_or(false);
    let msg_url = data
        .get("msgUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let scopes = data
        .get("scopes")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|s| BatchScope {
                    partition: s
                        .get("partition")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    from: json_u64(s.get("from")).unwrap_or(0),
                    to: json_u64(s.get("to")).unwrap_or(crate::config::JS_MAX_SAFE_INTEGER),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(UploadBatchPayload {
        sync_id,
        idx,
        is_last,
        err,
        batch_type,
        msg_url,
        scopes,
    })
}

/// Socket RPC retry policy matching Electron's `Wte` / `Kte.withRetry`.
///
/// - Max 3 attempts, backoff `min(1000 * 2^(attempt - 1), 3000)` ms (1000ms, 2000ms).
/// - `590` skips retry on `NO_NETWORK`, `TIMEOUT`, and error code `211`.
/// - `591` skips retry on `NO_NETWORK` and `TIMEOUT`.
/// - `592` retries all errors within the 3-attempt budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcErrorKind {
    /// Network down or socket not open.
    NoNetwork,
    /// Request timed out.
    Timeout,
    /// Server returned numeric error code (e.g., `211`).
    ServerCode(i64),
    /// Any other transport/parse failure.
    Other,
}

/// Computes `min(1000 * 2^(attempt - 1), 3000)` ms (`Kte.calculateBackoff`).
#[must_use]
pub fn retry_backoff_ms(attempt_1_based: u32) -> u64 {
    let shift = attempt_1_based.saturating_sub(1).min(10);
    1_000u64.saturating_mul(1u64 << shift).min(3_000)
}

/// Returns whether `cmd` (`590`, `591`, `592`) should retry after `err`.
#[must_use]
pub fn should_retry_cmd(cmd: u16, err: RpcErrorKind) -> bool {
    match cmd {
        CMD_SYNC_REQUEST => !matches!(
            err,
            RpcErrorKind::NoNetwork | RpcErrorKind::Timeout | RpcErrorKind::ServerCode(211)
        ),
        CMD_SYNC_ACK_DELETE => !matches!(err, RpcErrorKind::NoNetwork | RpcErrorKind::Timeout),
        CMD_SYNC_WAKEUP => true,
        _ => false,
    }
}

fn json_u64(v: Option<&Value>) -> Option<u64> {
    let v = v?;
    v.as_u64()
        .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn json_i64(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}
