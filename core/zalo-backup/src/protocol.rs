//! Wire-exact socket protocol definitions for PC → Phone Message Backup (`630`–`634`) and
//! control push events (`601`/`603`, `act_type == "backup_pc2mb"`).

use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{BackupRemoteConfig, ServerBackupConfigs};

/// Socket command: Init backup session with packed metadata + `bkid` + `client_salt`.
pub const CMD_BACKUP_INIT: u16 = 630;
/// Socket command: Create backup session (`bkid` + `server_nonce`).
pub const CMD_BACKUP_CREATE_SESSION: u16 = 631;
/// Socket command: Query existing backup metadata (`1001` = NONE, `0` = EXISTING).
pub const CMD_BACKUP_GET_METADATA: u16 = 632;
/// Socket command: Signal phone to start restoring from the uploaded backup.
pub const CMD_BACKUP_SIGNAL_RESTORE: u16 = 633;
/// Socket command: Fetch server backup configs (`configs.retention_time`).
pub const CMD_BACKUP_GET_CONFIGS: u16 = 634;

/// Default RPC timeout for `630`–`634` (`1e4` = 10s in `compact3857.js`).
pub const BACKUP_SOCKET_TIMEOUT_MS: u64 = 10_000;
/// Default retry delay for `631`/`630` retryable errors (`1e4` = 10s in `main-startup`).
pub const BACKUP_INIT_RETRY_DELAY_MS: u64 = 10_000;
/// Max attempts for `631`/`630` (`for (s = 1; s <= 3; s++)`).
pub const BACKUP_INIT_MAX_ATTEMPTS: u32 = 3;

/// Server error code from `632` meaning "no existing backup" (QoS success, status NONE).
pub const ERR_632_NO_BACKUP: i64 = 1001;
/// Server error code from `630` indicating invalid/expired session (clears session keys).
pub const ERR_630_INVALID_SESSION: i64 = 212;

/// Generates a fresh 32-byte cryptographically random `client_salt` encoded as standard base64,
/// matching `qg/u` export `b` in Electron (`crypto.getRandomValues(new Uint8Array(32))`).
#[must_use]
pub fn generate_client_salt() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Builds the JSON payload for `631`, `632`, `633`, or `634`: `{ "reqId": req_id }`.
#[must_use]
pub fn build_req_id_only_payload(req_id: &str) -> Value {
    json!({ "reqId": req_id })
}

/// Metadata sent inside `630` (`initBackupSession`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupInitMetadata {
    pub pc_device_name: String,
    pub min_msg_ts: u64,
    pub size: u64,
    pub original_size: u64,
    pub msg_count: u64,
    pub thread_count: u64,
    pub crypto_version: u32,
    pub db_format_version: u32,
    pub checksum: String,
}

/// Builds the wire JSON payload for socket `630`:
/// `{ "reqId": req_id, "data": { ...metadata, "bkid": bkid, "client_salt": client_salt } }`.
#[must_use]
pub fn build_init_630_payload(
    req_id: &str,
    meta: &BackupInitMetadata,
    bkid: &str,
    client_salt: &str,
) -> Value {
    json!({
        "reqId": req_id,
        "data": {
            "pc_device_name": meta.pc_device_name,
            "min_msg_ts": meta.min_msg_ts,
            "size": meta.size,
            "original_size": meta.original_size,
            "msg_count": meta.msg_count,
            "thread_count": meta.thread_count,
            "crypto_version": meta.crypto_version,
            "db_format_version": meta.db_format_version,
            "checksum": meta.checksum,
            "bkid": bkid,
            "client_salt": client_salt,
        }
    })
}

/// Parses socket `634` (`GET_BACKUP_CONFIGS`) response.
/// Returns `None` on non-zero `error_code` or missing `data.configs` (caller falls back to remote config).
#[must_use]
pub fn parse_634_configs(response: &Value) -> Option<ServerBackupConfigs> {
    let err_code = response.get("error_code").and_then(Value::as_i64)?;
    if err_code != 0 {
        return None;
    }
    let configs = response.pointer("/data/configs")?;
    if !configs.is_object() {
        return None;
    }
    let retention_time_ms = configs
        .get("retention_time")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(ServerBackupConfigs {
        retention_time_ms,
        raw: Some(configs.clone()),
    })
}

/// Existing backup metadata returned by socket `632`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistingBackupInfo {
    pub req_id: String,
    pub bkid: String,
    pub device_name: String,
    pub size: u64,
    pub created_time_ms: u64,
    pub expired_time_ms: u64,
    /// Effective TTL timestamp (`expired_time` if > 0 else `now + effective_ttl_ms`).
    pub ttl_ms: u64,
}

impl ExistingBackupInfo {
    /// Local expiry check (`Iy` in Electron): if `created_time_ms > 0` and
    /// `now_ms > created_time_ms + effective_ttl_ms`, the backup is treated as expired
    /// and the UI switches to the `Intro` popup.
    #[must_use]
    pub fn is_locally_expired(&self, now_ms: u64, effective_ttl_ms: u64) -> bool {
        self.created_time_ms > 0 && now_ms > self.created_time_ms.saturating_add(effective_ttl_ms)
    }
}

/// Result of parsing a socket `632` (`GET_METADATA`) response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupStatus632 {
    /// No backup exists (`error_code == 1001` → `qos_ok = true`; other error/missing data/exception → `qos_ok = false`).
    None { qos_ok: bool, error_code: i64 },
    /// An existing backup was found (`error_code == 0` with `data.metadata`).
    Existing(ExistingBackupInfo),
}

/// Exact port of Electron `getBackupStatus()` parsing (`main-startup` lines 37634–37700).
/// Never throws/returns `Err`: server errors or missing fields collapse to `BackupStatus632::None { qos_ok: false }`.
#[must_use]
pub fn parse_632_status(
    response: Option<&Value>,
    now_ms: u64,
    cfg: &BackupRemoteConfig,
    server_configs: Option<&ServerBackupConfigs>,
) -> BackupStatus632 {
    let Some(resp) = response else {
        return BackupStatus632::None {
            qos_ok: false,
            error_code: -1,
        };
    };
    let err_code = resp.get("error_code").and_then(Value::as_i64).unwrap_or(-1);
    let data = resp.get("data").filter(|v| v.is_object());

    if data.is_none() || err_code != 0 {
        if err_code == ERR_632_NO_BACKUP {
            return BackupStatus632::None {
                qos_ok: true,
                error_code: ERR_632_NO_BACKUP,
            };
        }
        return BackupStatus632::None {
            qos_ok: false,
            error_code: err_code,
        };
    }

    let data = data.unwrap();
    let meta = data.get("metadata").and_then(Value::as_object);
    let req_id = data
        .get("reqId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let bkid = meta
        .and_then(|m| m.get("bkid"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let device_name = meta
        .and_then(|m| m.get("pc_device_name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let size = meta
        .and_then(|m| m.get("size"))
        .and_then(value_as_u64)
        .unwrap_or(0);
    let created_time_ms = meta
        .and_then(|m| m.get("created_time"))
        .and_then(value_as_u64)
        .unwrap_or(0);
    let expired_time_ms = meta
        .and_then(|m| m.get("expired_time"))
        .and_then(value_as_u64)
        .unwrap_or(0);

    let effective_ttl = cfg.effective_backup_ttl_ms(server_configs);
    let ttl_ms = if expired_time_ms > 0 {
        expired_time_ms
    } else {
        now_ms.saturating_add(effective_ttl)
    };

    BackupStatus632::Existing(ExistingBackupInfo {
        req_id,
        bkid,
        device_name,
        size,
        created_time_ms,
        expired_time_ms,
        ttl_ms,
    })
}

/// Parsed response of socket `631` (`createBackupSession`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSession631 {
    pub req_id: String,
    pub bkid: String,
    pub server_nonce: String,
}

/// Error from socket `630`, `631`, or `633`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupRpcError {
    /// Non-zero outer `error_code` or inner `data.err` — NEVER retried by `631`/`630`.
    #[error("backup server error {code}: {message}")]
    Server { code: i64, message: String },
    /// Missing required response field (`bkid` or `server_nonce`).
    #[error("backup incomplete response: {0}")]
    Incomplete(String),
    /// Retryable transport/timeout/network failure.
    #[error("backup transport error ({code}): {message}")]
    Transport { code: String, message: String },
}

impl BackupRpcError {
    /// Returns the server error code if this is a [`BackupRpcError::Server`].
    #[must_use]
    pub fn server_error_code(&self) -> Option<i64> {
        match self {
            Self::Server { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// Exact port of Electron `isRetryableInitError` (`main-startup` line 37854).
    /// Server errors (`error_code != 0` or `data.err != 0`) are NEVER retried.
    #[must_use]
    pub fn is_retryable_init_error(&self) -> bool {
        match self {
            Self::Server { .. } | Self::Incomplete(_) => false,
            Self::Transport { code, message } => is_retryable_init_code_or_msg(code, message),
        }
    }
}

/// Exact predicate matching Electron `isRetryableInitError(err)`.
#[must_use]
pub fn is_retryable_init_code_or_msg(code: &str, message: &str) -> bool {
    let c = code.trim();
    if matches!(
        c,
        "-3" | "-2"
            | "ERR_CONNECTION_TIMED_OUT"
            | "ECONNABORTED"
            | "ETIMEDOUT"
            | "ERR_NO_NETWORK"
            | "NO_NETWORK"
            | "ENETUNREACH"
    ) {
        return true;
    }
    let m = message.to_ascii_lowercase();
    m.contains("timeout") || m.contains("timed out") || m.contains("network")
}

/// Parses socket `631` (`createBackupSession`) response.
/// Rejects non-zero outer `error_code`, non-zero `data.err`, or missing `bkid` / `server_nonce`.
pub fn parse_631_response(response: &Value) -> Result<BackupSession631, BackupRpcError> {
    let outer = response
        .get("error_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let data = response.get("data").filter(|v| v.is_object());
    let inner_err = data.and_then(|d| d.get("err")).and_then(Value::as_i64);

    if outer != 0 || inner_err != Some(0) {
        let code = inner_err.filter(|&e| e != 0).unwrap_or(outer);
        return Err(BackupRpcError::Server {
            code,
            message: format!("createBackupSession failed with error: {code}"),
        });
    }
    let data = data.unwrap();
    let bkid = data
        .get("bkid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            BackupRpcError::Incomplete(
                "createBackupSession: missing bkid or server_nonce in response".into(),
            )
        })?;
    let server_nonce = data
        .get("server_nonce")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            BackupRpcError::Incomplete(
                "createBackupSession: missing bkid or server_nonce in response".into(),
            )
        })?;
    let req_id = data
        .get("reqId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Ok(BackupSession631 {
        req_id,
        bkid: bkid.to_owned(),
        server_nonce: server_nonce.to_owned(),
    })
}

/// Parsed response of socket `630` (`initBackupSession`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInit630 {
    pub req_id: String,
    pub bkid: String,
}

/// Parses socket `630` (`initBackupSession`) response.
/// Rejects non-zero outer `error_code`, non-zero `data.err`, or missing `bkid`.
pub fn parse_630_response(response: &Value) -> Result<BackupInit630, BackupRpcError> {
    let outer = response
        .get("error_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let data = response.get("data").filter(|v| v.is_object());
    let inner_err = data.and_then(|d| d.get("err")).and_then(Value::as_i64);

    if outer != 0 || inner_err != Some(0) {
        let code = inner_err.filter(|&e| e != 0).unwrap_or(outer);
        return Err(BackupRpcError::Server {
            code,
            message: format!("initBackupSession failed with error: {code}"),
        });
    }
    let data = data.unwrap();
    let bkid = data
        .get("bkid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            BackupRpcError::Incomplete("initBackupSession: missing bkid in response".into())
        })?;
    let req_id = data
        .get("reqId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Ok(BackupInit630 {
        req_id,
        bkid: bkid.to_owned(),
    })
}

/// Parses socket `633` (`signalRestoreOnMobile`) response.
/// Succeeds ONLY when outer `error_code == 0` AND `data.err == 0`.
pub fn parse_633_response(response: &Value) -> Result<(), BackupRpcError> {
    let outer = response
        .get("error_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let inner_err = response
        .pointer("/data/err")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    if outer != 0 || inner_err != 0 {
        let code = if inner_err != 0 { inner_err } else { outer };
        return Err(BackupRpcError::Server {
            code,
            message: format!("signalRestoreOnMobile failed with error: {code}"),
        });
    }
    Ok(())
}

/// Control events routed from socket `601`/`603` when `act_type == "backup_pc2mb"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupControlEvent {
    /// `act == "backup_done"` — matched by `bkid` for QoS monitor ONLY.
    BackupDone { bkid: String, err: i64 },
    /// `act == "mb_start_restore"` — mobile has begun restoring.
    MobileStartRestore { bkid: String },
}

/// Extracts `backup_pc2mb` control events from a socket `601`/`603` `controls` array.
#[must_use]
pub fn parse_backup_controls(controls: &[Value]) -> Vec<BackupControlEvent> {
    let mut out = Vec::new();
    for ctrl in controls {
        let Some(content) = ctrl.get("content") else {
            continue;
        };
        let act_type = content
            .get("act_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        if act_type != "backup_pc2mb" {
            continue;
        }
        let act = content.get("act").and_then(Value::as_str).unwrap_or("");
        let data_val = match content.get("data") {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
            Some(v) => v.clone(),
            None => Value::Null,
        };
        let bkid = data_val
            .get("bkid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        match act {
            "backup_done" => {
                let err = data_val.get("err").and_then(Value::as_i64).unwrap_or(0);
                out.push(BackupControlEvent::BackupDone { bkid, err });
            }
            "mb_start_restore" => {
                out.push(BackupControlEvent::MobileStartRestore { bkid });
            }
            _ => {}
        }
    }
    out
}

fn value_as_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
}
