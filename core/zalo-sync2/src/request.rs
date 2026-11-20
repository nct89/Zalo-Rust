//! Socket request bodies (`cmd 590/591/592`) and `transfer_sync2` control events.

use serde_json::{json, Value};

/// `SYNC_MESSAGE.REQUEST`: ask the phone for data.
pub const CMD_REQUEST_SYNC: u16 = 590;
/// `SYNC_MESSAGE.ACK_DELETE_SYNC_SESSION`: close a session.
pub const CMD_ACK_DELETE_SESSION: u16 = 591;
/// `SYNC_MESSAGE.REQUEST_MOBILE_WAKE_UP`: push-wake the phone.
pub const CMD_REQUEST_WAKE_UP: u16 = 592;

/// `act_type` of sync v2 control events.
pub const ACT_TYPE_SYNC2: &str = "transfer_sync2";
/// `act_type` of sync 1 control events; also routed to sync v2 when it is on.
pub const ACT_TYPE_SYNC1: &str = "transfer_msg";

/// `limit` meaning "everything".
pub const NO_LIMIT: u64 = 2_147_483_647;
/// `to` meaning "no upper bound" (`Number.MAX_SAFE_INTEGER`).
pub const NO_UPPER_BOUND: u64 = 9_007_199_254_740_991;
/// Default `batchSize` (`transfer_batch_size`).
pub const DEFAULT_BATCH_SIZE: u32 = 2000;

/// Which database on the phone a request reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Database {
    /// Conversation list (`partition` is empty, range is on `lastTs`).
    Conversation,
    /// Messages (one `partition` per conversation, range is on `timestamp`).
    Message,
}

impl Database {
    /// Name sent in `req.type`.
    pub fn name(self) -> &'static str {
        match self {
            Database::Conversation => "conversation",
            Database::Message => "message",
        }
    }
}

/// `partition` of a conversation: `oneone/<globalId>` or `group/<globalId>`.
pub fn partition(global_id: &str, is_group: bool) -> String {
    format!("{}/{global_id}", if is_group { "group" } else { "oneone" })
}

/// Splits a `partition` back into `(global_id, is_group)`.
pub fn parse_partition(partition: &str) -> Option<(&str, bool)> {
    if let Some(id) = partition.strip_prefix("group/") {
        Some((id, true))
    } else {
        partition.strip_prefix("oneone/").map(|id| (id, false))
    }
}

/// One entry of `req.queries`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// See [`partition`]; empty for [`Database::Conversation`].
    pub partition: String,
    /// Lower bound, Unix ms.
    pub from: u64,
    /// Upper bound, Unix ms.
    pub to: u64,
    /// Maximum number of rows.
    pub limit: u64,
}

impl Query {
    /// Every message of one conversation with `from <= timestamp <= to`.
    pub fn messages(global_id: &str, is_group: bool, from: u64, to: u64) -> Self {
        Self {
            partition: partition(global_id, is_group),
            from,
            to,
            limit: NO_LIMIT,
        }
    }

    /// The whole conversation list.
    pub fn conversations() -> Self {
        Self {
            partition: String::new(),
            from: 0,
            to: NO_UPPER_BOUND,
            limit: NO_LIMIT,
        }
    }
}

/// Parameters of one `cmd 590` request.
#[derive(Clone, Debug)]
pub struct SyncRequest {
    /// 32-character random session id.
    pub sync_id: String,
    /// Base64 ephemeral public key (`ek`).
    pub ek: String,
    /// Base64 identity public key (`ik`).
    pub ik: String,
    /// `temp_key` from a recent `transfer_after_login`, or empty.
    pub temp_key: String,
    /// Name shown on the phone.
    pub device_name: String,
    /// Database to read.
    pub database: Database,
    /// Priority; one value per request.
    pub priority: u32,
    /// Queries.
    pub queries: Vec<Query>,
    /// Rows per batch.
    pub batch_size: u32,
    /// Index of this request in the user sync session.
    pub ussidx: u32,
    /// `true` for `SyncOnDemand` (1), `false` for `SyncFull` (0).
    pub on_demand: bool,
}

impl SyncRequest {
    /// Body of `cmd 590`: `{"data": {...}}`.
    pub fn body(&self) -> Value {
        let queries: Vec<Value> = self
            .queries
            .iter()
            .map(
                |q| json!({"partition": q.partition, "from": q.from, "to": q.to, "limit": q.limit}),
            )
            .collect();
        json!({
            "data": {
                "syncId": self.sync_id,
                "syncType": u8::from(self.on_demand),
                "ek": self.ek,
                "ik": self.ik,
                "toDevice": DEVICE_MOBILE,
                "tempKey": self.temp_key,
                "deviceName": self.device_name,
                "req": {
                    "type": self.database.name(),
                    "priority": self.priority,
                    "queries": queries,
                    "batchSize": self.batch_size,
                },
                "ver": 1,
                "ussidx": self.ussidx,
            }
        })
    }
}

/// `toDevice` / `fromDevice` value of the phone.
pub const DEVICE_MOBILE: u8 = 0;

/// Body of `cmd 592`.
pub fn wake_up_body(sync_id: &str) -> Value {
    json!({"syncId": sync_id, "toDevice": DEVICE_MOBILE})
}

/// Body of `cmd 591`.
pub fn dispose_body(sync_id: &str, reason: i64) -> Value {
    json!({"data": {"syncId": sync_id, "toDevice": DEVICE_MOBILE, "reason": reason}})
}

/// Random 32-character session id (letters and digits, like `generateRandomString(32)`).
pub fn new_sync_id() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rngs::OsRng;
    (0..32)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

/// `status` of a `transfer_status` event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobileStatus {
    /// 1: the phone is online and working.
    Active,
    /// 2: the phone went idle.
    Idle,
    /// 3: the phone is showing the confirm dialog.
    WaitingConfirm,
    /// 4: the user accepted.
    Confirmed,
    /// 5: accepted without a dialog (valid `tempKey`).
    BypassConfirmed,
    /// 6: the user declined.
    UserReject,
    /// 7: the user cancelled.
    UserCancel,
    /// 8: the phone is busy with another sync.
    Busy,
    /// 9: the phone is low on storage.
    LowStorage,
    /// 10: the phone logged out.
    Logout,
    /// Any other value.
    Other(i64),
}

impl MobileStatus {
    fn from_code(code: i64) -> Self {
        match code {
            1 => Self::Active,
            2 => Self::Idle,
            3 => Self::WaitingConfirm,
            4 => Self::Confirmed,
            5 => Self::BypassConfirmed,
            6 => Self::UserReject,
            7 => Self::UserCancel,
            8 => Self::Busy,
            9 => Self::LowStorage,
            10 => Self::Logout,
            other => Self::Other(other),
        }
    }

    /// `true` when the session can no longer succeed.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::UserReject | Self::UserCancel | Self::Busy | Self::LowStorage | Self::Logout
        )
    }
}

/// `batchType` of a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchType {
    /// 1: frames decrypted with the session record.
    Message,
    /// 2: frames that carry the session record.
    MetadataEmbedded,
    /// Any other value.
    Other(i64),
}

/// One `scopes` entry of a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// Partition.
    pub partition: String,
    /// Lower bound covered by the batch.
    pub from: u64,
    /// Upper bound covered by the batch.
    pub to: u64,
}

/// An `upload_batch` or `transfer_error` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchInfo {
    /// Session id.
    pub sync_id: String,
    /// Batch index, from 0.
    pub idx: u32,
    /// Whether this is the last batch.
    pub is_last: bool,
    /// 0 on success.
    pub err: i64,
    /// Batch type.
    pub batch_type: BatchType,
    /// URL of the batch file.
    pub msg_url: String,
    /// Ranges the batch covers.
    pub scopes: Vec<Scope>,
}

/// A decoded sync control event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlEvent {
    /// `transfer_after_login`: a `temp_key` for the next session.
    TransferAfterLogin {
        /// One-time key.
        temp_key: String,
        /// `z_uuid` of the PC it is meant for.
        imei: String,
        /// Event time, Unix ms.
        ts: i64,
    },
    /// `transfer_status` from the phone.
    Status {
        /// Session id.
        sync_id: String,
        /// Status.
        status: MobileStatus,
    },
    /// `upload_batch` with `err = 0`.
    Batch(BatchInfo),
    /// `upload_batch` with an error, or `transfer_error`.
    BatchError(BatchInfo),
}

fn as_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_f64().map(|f| f as i64))
}

fn as_u64(v: Option<&Value>) -> u64 {
    v.and_then(as_i64).map_or(0, |n| n.max(0) as u64)
}

fn str_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn batch_info(data: &Value, err_field: &str) -> BatchInfo {
    let scopes = data
        .get("scopes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|s| Scope {
                    partition: str_of(s.get("partition")),
                    from: as_u64(s.get("from")),
                    to: s
                        .get("to")
                        .and_then(as_i64)
                        .map_or(NO_UPPER_BOUND, |n| n.max(0) as u64),
                })
                .collect()
        })
        .unwrap_or_default();
    BatchInfo {
        sync_id: str_of(data.get("syncId")),
        idx: as_u64(data.get("idx")) as u32,
        is_last: data
            .get("isLast")
            .is_some_and(|v| v.as_bool().unwrap_or_else(|| as_i64(v) == Some(1))),
        err: data.get(err_field).and_then(as_i64).unwrap_or(0),
        batch_type: match data.get("batchType").and_then(as_i64) {
            Some(1) => BatchType::Message,
            Some(2) => BatchType::MetadataEmbedded,
            other => BatchType::Other(other.unwrap_or(0)),
        },
        msg_url: str_of(data.get("msgUrl")),
        scopes,
    }
}

/// Reads one entry of a `cmd 601` `controls` list.
///
/// Returns `None` for events of other features, for `transfer_status` events
/// that do not come from the phone, and for unknown sync acts. `data` may be a
/// JSON object or a JSON string.
pub fn parse_control(control: &Value) -> Option<ControlEvent> {
    let act_type = control.get("act_type").and_then(Value::as_str)?;
    if act_type != ACT_TYPE_SYNC2 && act_type != ACT_TYPE_SYNC1 {
        return None;
    }
    let parsed;
    let data = match control.get("data") {
        Some(Value::String(text)) => {
            parsed = serde_json::from_str::<Value>(text).ok()?;
            &parsed
        }
        Some(other) => other,
        None => return None,
    };
    match control.get("act").and_then(Value::as_str)? {
        "transfer_after_login" => {
            let temp_key = str_of(data.get("temp_key"));
            (!temp_key.is_empty()).then(|| ControlEvent::TransferAfterLogin {
                temp_key,
                imei: str_of(data.get("imei")),
                ts: control.get("ts").and_then(as_i64).unwrap_or(0),
            })
        }
        "transfer_status" => {
            let from = data.get("fromDevice").and_then(as_i64).unwrap_or(0);
            (from == i64::from(DEVICE_MOBILE)).then(|| ControlEvent::Status {
                sync_id: str_of(data.get("syncId")),
                status: MobileStatus::from_code(data.get("status").and_then(as_i64).unwrap_or(0)),
            })
        }
        "upload_batch" => {
            let info = batch_info(data, "err");
            Some(if info.err == 0 {
                ControlEvent::Batch(info)
            } else {
                ControlEvent::BatchError(info)
            })
        }
        "transfer_error" => Some(ControlEvent::BatchError(batch_info(data, "errorCode"))),
        _ => None,
    }
}
