//! End-to-end PC → Phone Message Backup pipeline (`634` → `632` → `631` → pack → `630` →
//! chunked upload → `hy` verify → Success banner → QoS-only `backup_done` monitor, plus
//! separate user-triggered `633` and auto-resume helpers).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{BackupRemoteConfig, ServerBackupConfigs, UPLOAD_REUSE_WINDOW_MS};
use crate::guard::BackupErrorCategory;
use crate::packer::{compute_file_sha256_and_size, compute_ui_upload_percent, PackResult};
use crate::protocol::{
    generate_client_salt, parse_630_response, parse_631_response, parse_632_status,
    parse_633_response, parse_634_configs, BackupInit630, BackupInitMetadata, BackupRpcError,
    BackupSession631, BackupStatus632, BACKUP_INIT_MAX_ATTEMPTS, BACKUP_INIT_RETRY_DELAY_MS,
    ERR_630_INVALID_SESSION,
};

/// Chunk retry budget (`Mb = q.SEND_CHUNK + 1 = 2`, checked as `n <= 0` then decremented → 3 attempts).
pub const CHUNK_UPLOAD_EXTRA_RETRIES: u32 = 2;
/// Base HTTP chunk timeout (`60_000` ms).
pub const CHUNK_UPLOAD_TIMEOUT_START_MS: u64 = 60_000;
/// Timeout increment on each timeout retry (`60_000` ms, capped at `120_000` ms).
pub const CHUNK_UPLOAD_TIMEOUT_MAX_MS: u64 = 120_000;

/// Timing configuration for pipeline retries (overridable in unit tests so tests run in milliseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupTimingConfig {
    pub init_retry_delay_ms: u64,
    pub chunk_timeout_retry_delay_ms: u64,
    pub chunk_server_retry_delay_ms: u64,
    pub network_wait_max_ms: u64,
    pub network_poll_interval_ms: u64,
}

impl Default for BackupTimingConfig {
    fn default() -> Self {
        Self {
            init_retry_delay_ms: BACKUP_INIT_RETRY_DELAY_MS,
            chunk_timeout_retry_delay_ms: 1_000,
            chunk_server_retry_delay_ms: 10_000,
            network_wait_max_ms: 50_000,
            network_poll_interval_ms: 5_000,
        }
    }
}

/// Request passed to [`BackupTransport::upload_chunk`].
#[derive(Debug, Clone)]
pub struct UploadChunkRequest<'a> {
    pub bkid: &'a str,
    /// 1-based chunk index.
    pub chunk_id: u32,
    pub total_chunks: u32,
    /// `true` iff `chunk_id == total_chunks` (`last: 1` vs `0` in `batch_data`).
    pub is_last: bool,
    /// Total backup file size in bytes (sent in `batch_data.size`).
    pub file_size: u64,
    /// Raw chunk bytes (`file_data`, filename `"backup_chunk"`, `application/octet-stream`).
    pub bytes: &'a [u8],
    /// Logged ascending timeout (`60_000` → `120_000` ms).
    pub logged_timeout_ms: u64,
}

/// Classification of an HTTP chunk upload failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UploadChunkError {
    #[error("chunk upload timed out ({code}): {message}")]
    Timeout { code: String, message: String },
    #[error("chunk upload network error ({code}): {message}")]
    Network { code: String, message: String },
    #[error("chunk upload server error ({code:?}): {message}")]
    Server {
        server_code: Option<i64>,
        code: String,
        message: String,
    },
    #[error("chunk upload fatal error: {0}")]
    Fatal(String),
}

impl UploadChunkError {
    /// Classifies an arbitrary code/status/message into Electron's 3 retry classes vs fatal.
    #[must_use]
    pub fn classify(
        server_code: Option<i64>,
        code: &str,
        message: &str,
        is_network_err: bool,
    ) -> Self {
        let c = code.trim();
        let m_upper = message.to_ascii_uppercase();
        if matches!(c, "ERR_CONNECTION_TIMED_OUT" | "ECONNABORTED")
            || m_upper.contains("ERR_CONNECTION_TIMED_OUT")
            || m_upper.contains("ECONNABORTED")
        {
            return Self::Timeout {
                code: c.to_owned(),
                message: message.to_owned(),
            };
        }
        if is_network_err
            || matches!(c, "ERR_NO_NETWORK" | "NO_NETWORK_ERR" | "NO_NETWORK")
            || m_upper.contains("ERR_NO_NETWORK")
            || m_upper.contains("NO_NETWORK")
        {
            return Self::Network {
                code: c.to_owned(),
                message: message.to_owned(),
            };
        }
        if matches!(server_code, Some(112) | Some(220))
            || c == "in2_112"
            || message.contains("ERR_REQUEST_ABORT")
        {
            return Self::Server {
                server_code,
                code: c.to_owned(),
                message: message.to_owned(),
            };
        }
        Self::Fatal(if message.is_empty() {
            format!("upload failed (code={c}, server_code={server_code:?})")
        } else {
            message.to_owned()
        })
    }

    /// Returns whether this error is one of Electron's 3 retryable chunk upload classes.
    #[must_use]
    pub fn retry_kind(&self) -> Option<ChunkRetryKind> {
        match self {
            Self::Timeout { .. } => Some(ChunkRetryKind::Timeout),
            Self::Network { .. } => Some(ChunkRetryKind::Network),
            Self::Server {
                server_code,
                code,
                message,
            } if matches!(server_code, Some(112) | Some(220))
                || code == "in2_112"
                || message.contains("ERR_REQUEST_ABORT") =>
            {
                Some(ChunkRetryKind::ServerTransient)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkRetryKind {
    Timeout,
    Network,
    ServerTransient,
}

/// Transport abstraction for socket `630`–`634` + HTTP `/api/backuppc2mb/upload`.
/// All futures are `Send` so the pipeline runs cleanly on multi-threaded Tokio executors.
pub trait BackupTransport: Send + Sync {
    fn send_634_get_configs(
        &self,
        req_id: &str,
    ) -> impl Future<Output = Result<Value, BackupRpcError>> + Send;

    fn send_632_get_metadata(
        &self,
        req_id: &str,
    ) -> impl Future<Output = Result<Value, BackupRpcError>> + Send;

    fn send_631_create_session(
        &self,
        req_id: &str,
    ) -> impl Future<Output = Result<Value, BackupRpcError>> + Send;

    fn send_630_init_session(
        &self,
        req_id: &str,
        meta: &BackupInitMetadata,
        bkid: &str,
        client_salt: &str,
    ) -> impl Future<Output = Result<Value, BackupRpcError>> + Send;

    fn send_633_signal_restore(
        &self,
        req_id: &str,
    ) -> impl Future<Output = Result<Value, BackupRpcError>> + Send;

    fn upload_chunk<'a>(
        &'a self,
        req: &'a UploadChunkRequest<'a>,
    ) -> impl Future<Output = Result<(), UploadChunkError>> + Send + 'a;

    fn is_online(&self) -> bool;
}

/// Pipeline-level errors with clean mapping to Electron banner/popup categories.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupPipelineError {
    #[error("backup feature is disabled by remote config")]
    FeatureDisabled,
    #[error("encryption enabled without valid server_nonce or client_salt (CMD 631 must run before engine)")]
    MissingEncryptionKeys,
    #[error("backup has 0 message scopes")]
    EmptyScopes,
    #[error("backup integrity check failed: {0}")]
    Integrity(String),
    #[error("backup file size {size} exceeds max_file_size {limit}")]
    FileTooLarge { size: u64, limit: u64 },
    #[error("CMD 630 upload window expired")]
    UploadWindowExpired,
    #[error("backup RPC failed: {0}")]
    Rpc(#[from] BackupRpcError),
    #[error("chunk upload failed: {0}")]
    Upload(#[from] UploadChunkError),
    #[error("backup i/o error: {0}")]
    Io(String),
}

impl BackupPipelineError {
    /// Maps the pipeline error to the Electron sidebar banner category.
    #[must_use]
    pub fn banner_category(&self) -> BackupErrorCategory {
        match self {
            Self::EmptyScopes => BackupErrorCategory::UserSystem,
            Self::FileTooLarge { .. } => BackupErrorCategory::FullStorage,
            Self::Upload(UploadChunkError::Network { .. } | UploadChunkError::Timeout { .. }) => {
                BackupErrorCategory::Network
            }
            Self::Rpc(e) if e.is_retryable_init_error() => BackupErrorCategory::Network,
            _ => BackupErrorCategory::Other,
        }
    }
}

/// Mutable in-memory backup session state carried across `634`/`632`/`631`/pack/`630`/upload.
#[derive(Debug, Clone, Default)]
pub struct BackupSessionState {
    pub bkid: Option<String>,
    pub server_nonce: Option<String>,
    pub client_salt: Option<String>,
    /// Timestamp when `630` succeeded (used for `oy = 1_740_000` ms upload reuse window).
    pub bkid_created_at_ms: Option<u64>,
    /// 1-based index of the last chunk successfully uploaded (0 = none yet).
    pub last_uploaded_chunk_id: u32,
    pub file_path: Option<PathBuf>,
    pub metadata: Option<BackupInitMetadata>,
    pub server_configs: Option<ServerBackupConfigs>,
    pub server_configs_fetched_at_ms: Option<u64>,
    pub last_success_at_ms: Option<u64>,
}

impl BackupSessionState {
    /// Clears `bkid`, `server_nonce`, `client_salt`, `bkid_created_at_ms`, and
    /// `last_uploaded_chunk_id` (used on `630` window expiry, `630` error `212`, or `scopeCount == 0`).
    pub fn clear_session_keys(&mut self) {
        self.bkid = None;
        self.server_nonce = None;
        self.client_salt = None;
        self.bkid_created_at_ms = None;
        self.last_uploaded_chunk_id = 0;
    }

    /// Returns whether the session has valid reusable `631` keys before running the packer engine
    /// (`oy = 1_740_000` rule).
    #[must_use]
    pub fn can_reuse_before_engine(&self, now_ms: u64) -> bool {
        let has_keys = self.bkid.as_deref().is_some_and(|s| !s.is_empty())
            && self.server_nonce.as_deref().is_some_and(|s| !s.is_empty())
            && self.client_salt.as_deref().is_some_and(|s| !s.is_empty());
        if !has_keys {
            return false;
        }
        match self.bkid_created_at_ms {
            None => true,
            Some(created) => now_ms.saturating_sub(created) <= UPLOAD_REUSE_WINDOW_MS,
        }
    }
}

/// Fetches fresh server configs (`634`) when cache is stale (always stale when default TTL = -1),
/// then queries `632` (`GET_METADATA`). Never throws: `634` errors fall back to `None`, and
/// `632` errors collapse to `BackupStatus632::None { qos_ok: false }`.
pub async fn fetch_configs_and_status<T: BackupTransport>(
    transport: &T,
    state: &mut BackupSessionState,
    cfg: &BackupRemoteConfig,
    now_ms: u64,
    req_id_634: &str,
    req_id_632: &str,
) -> BackupStatus632 {
    if !cfg.is_server_configs_fresh(state.server_configs_fetched_at_ms, now_ms) {
        if let Ok(resp) = transport.send_634_get_configs(req_id_634).await {
            if let Some(parsed) = parse_634_configs(&resp) {
                state.server_configs = Some(parsed);
                state.server_configs_fetched_at_ms = Some(now_ms);
            }
        }
    }
    let resp_632 = transport.send_632_get_metadata(req_id_632).await.ok();
    parse_632_status(resp_632.as_ref(), now_ms, cfg, state.server_configs.as_ref())
}

/// Calls socket `631` (`createBackupSession`) with up to 3 attempts on retryable errors only.
/// Server errors (`error_code != 0` or `data.err != 0`) and incomplete payloads fail immediately.
pub async fn call_631_with_retry<T: BackupTransport>(
    transport: &T,
    timing: BackupTimingConfig,
    mut make_req_id: impl FnMut(u32) -> String,
) -> Result<BackupSession631, BackupRpcError> {
    let mut last_err = None;
    for attempt in 1..=BACKUP_INIT_MAX_ATTEMPTS {
        let req_id = make_req_id(attempt);
        match transport.send_631_create_session(&req_id).await {
            Ok(resp) => match parse_631_response(&resp) {
                Ok(sess) => return Ok(sess),
                Err(e) => {
                    // Server error or incomplete payload: never retried!
                    return Err(e);
                }
            },
            Err(e) => {
                let retryable = e.is_retryable_init_error();
                last_err = Some(e);
                if !retryable || attempt >= BACKUP_INIT_MAX_ATTEMPTS {
                    break;
                }
                if timing.init_retry_delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(timing.init_retry_delay_ms)).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| BackupRpcError::Incomplete("631 exhausted".into())))
}

/// Ensures a valid `bkid` + `server_nonce` + local `client_salt` exist before launching the
/// packing worker (`oy = 1_740_000` ms reuse rule).
/// When `cfg.encryption_enabled == true`, guarantees non-empty `server_nonce` and `client_salt`
/// (NEVER falls back to raw!).
pub async fn ensure_session_before_engine<T: BackupTransport>(
    transport: &T,
    state: &mut BackupSessionState,
    cfg: &BackupRemoteConfig,
    now_ms: u64,
    timing: BackupTimingConfig,
    make_req_id: impl FnMut(u32) -> String,
) -> Result<(String, String, String), BackupPipelineError> {
    if !state.can_reuse_before_engine(now_ms) {
        let sess = call_631_with_retry(transport, timing, make_req_id).await?;
        let salt = generate_client_salt();
        state.bkid = Some(sess.bkid);
        state.server_nonce = Some(sess.server_nonce);
        state.client_salt = Some(salt);
        state.bkid_created_at_ms = None;
        state.last_uploaded_chunk_id = 0;
    }

    let bkid = state.bkid.clone().unwrap_or_default();
    let nonce = state.server_nonce.clone().unwrap_or_default();
    let salt = state.client_salt.clone().unwrap_or_default();
    if cfg.encryption_enabled && (bkid.is_empty() || nonce.is_empty() || salt.is_empty()) {
        return Err(BackupPipelineError::MissingEncryptionKeys);
    }
    Ok((bkid, nonce, salt))
}

/// Builds `BackupInitMetadata` from a [`PackResult`] and validates pre-`630` integrity.
/// - `scope_count == 0`: clears session keys and returns `EmptyScopes` (`UserSystem` banner).
/// - `item_count == 0` while `scope_count > 0`: returns `Integrity` error.
/// - `file_size == 0` or empty `checksum`: returns `Integrity` error.
pub fn build_and_validate_metadata(
    pack: &PackResult,
    pc_device_name: &str,
    cfg: &BackupRemoteConfig,
    state: &mut BackupSessionState,
) -> Result<BackupInitMetadata, BackupPipelineError> {
    if pack.scope_count == 0 {
        state.clear_session_keys();
        return Err(BackupPipelineError::EmptyScopes);
    }
    if pack.item_count == 0 {
        return Err(BackupPipelineError::Integrity(
            "item_count is 0 while scope_count > 0".into(),
        ));
    }
    if pack.file_size == 0 {
        return Err(BackupPipelineError::Integrity(
            "backup file size is 0".into(),
        ));
    }
    if pack.checksum.is_empty() {
        return Err(BackupPipelineError::Integrity(
            "backup checksum is empty".into(),
        ));
    }

    let meta = BackupInitMetadata {
        pc_device_name: pc_device_name.to_owned(),
        min_msg_ts: 0,
        size: pack.file_size,
        original_size: pack.original_size,
        msg_count: pack.item_count,
        thread_count: pack.scope_count,
        crypto_version: cfg.crypto_version,
        db_format_version: cfg.db_format_version,
        checksum: pack.checksum.clone(),
    };

    // Double-check strict match against worker output (Electron pre-630 integrity check).
    if meta.msg_count != pack.item_count
        || meta.thread_count != pack.scope_count
        || meta.size != pack.file_size
    {
        return Err(BackupPipelineError::Integrity(
            "metadata fields do not match packer result".into(),
        ));
    }

    state.file_path = Some(pack.file_path.clone());
    state.metadata = Some(meta.clone());
    Ok(meta)
}

/// Runs socket `630` (`initBackupSession`) with exact Electron reuse window (`oy = 1_740_000` ms)
/// and error `212` key-clearing semantics:
/// - If `bkid` present and `bkid_created_at_ms` known and age ≤ `oy` → skip `630`, return `bkid`.
/// - If `bkid_created_at_ms` known and age > `oy` → clear session keys & return `UploadWindowExpired`.
/// - If `bkid_created_at_ms` is `None` → send `630` (max 3 attempts on retryable errors).
///   - Server error `212` clears session keys before returning `Err`.
pub async fn ensure_init_630<T: BackupTransport>(
    transport: &T,
    state: &mut BackupSessionState,
    meta: &BackupInitMetadata,
    now_ms: u64,
    timing: BackupTimingConfig,
    mut make_req_id: impl FnMut(u32) -> String,
) -> Result<BackupInit630, BackupPipelineError> {
    if let (Some(bkid), Some(created_at)) = (&state.bkid, state.bkid_created_at_ms) {
        if !bkid.is_empty() {
            if now_ms.saturating_sub(created_at) <= UPLOAD_REUSE_WINDOW_MS {
                return Ok(BackupInit630 {
                    req_id: String::new(),
                    bkid: bkid.clone(),
                });
            }
            // Upload window expired: clear keys and fail (next run restarts from 631).
            state.clear_session_keys();
            return Err(BackupPipelineError::UploadWindowExpired);
        }
    }

    let bkid = state
        .bkid
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or(BackupPipelineError::MissingEncryptionKeys)?;
    let client_salt = state.client_salt.clone().unwrap_or_default();

    let mut last_err = None;
    for attempt in 1..=BACKUP_INIT_MAX_ATTEMPTS {
        let req_id = make_req_id(attempt);
        match transport
            .send_630_init_session(&req_id, meta, &bkid, &client_salt)
            .await
        {
            Ok(resp) => match parse_630_response(&resp) {
                Ok(ok) => {
                    state.bkid = Some(ok.bkid.clone());
                    state.bkid_created_at_ms = Some(now_ms);
                    return Ok(ok);
                }
                Err(e) => {
                    if e.server_error_code() == Some(ERR_630_INVALID_SESSION) {
                        state.clear_session_keys();
                    }
                    return Err(BackupPipelineError::Rpc(e));
                }
            },
            Err(e) => {
                if e.server_error_code() == Some(ERR_630_INVALID_SESSION) {
                    state.clear_session_keys();
                }
                let retryable = e.is_retryable_init_error();
                last_err = Some(e);
                if !retryable || attempt >= BACKUP_INIT_MAX_ATTEMPTS {
                    break;
                }
                if timing.init_retry_delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(timing.init_retry_delay_ms)).await;
                }
            }
        }
    }
    Err(BackupPipelineError::Rpc(last_err.unwrap_or_else(|| {
        BackupRpcError::Incomplete("630 exhausted".into())
    })))
}

/// Outcome of a completed + `hy`-verified chunked backup upload.
/// NOTE: Does NOT send socket `633`! `633` is strictly triggered by user buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupUploadOutcome {
    pub bkid: String,
    pub file_size: u64,
    pub total_chunks: u32,
}

/// Uploads the packed backup file in chunks of `cfg.chunk_size` (default 3 MiB), resuming at
/// `state.last_uploaded_chunk_id + 1`, with Electron's exact 3-attempt / 3-error-class retry loop
/// and post-upload `hy` integrity verification.
pub async fn upload_backup_file_chunked<T: BackupTransport>(
    transport: &T,
    state: &mut BackupSessionState,
    cfg: &BackupRemoteConfig,
    timing: BackupTimingConfig,
    mut on_ui_percent: impl FnMut(u32),
) -> Result<BackupUploadOutcome, BackupPipelineError> {
    let bkid = state
        .bkid
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or(BackupPipelineError::MissingEncryptionKeys)?;
    let file_path = state
        .file_path
        .clone()
        .ok_or_else(|| BackupPipelineError::Integrity("missing backup file path".into()))?;

    let bytes = std::fs::read(&file_path)
        .map_err(|e| BackupPipelineError::Io(format!("read backup file failed: {e}")))?;
    let file_size = bytes.len() as u64;
    if file_size == 0 {
        return Err(BackupPipelineError::Integrity("backup file is empty".into()));
    }
    if cfg.max_file_size > 0 && file_size > cfg.max_file_size {
        return Err(BackupPipelineError::FileTooLarge {
            size: file_size,
            limit: cfg.max_file_size,
        });
    }

    let chunk_size = cfg.chunk_size.max(1);
    let total_chunks = ((file_size as usize).div_ceil(chunk_size)) as u32;
    let start_chunk = state.last_uploaded_chunk_id.saturating_add(1);

    for chunk_id in start_chunk..=total_chunks {
        let start = ((chunk_id - 1) as usize) * chunk_size;
        let end = (start + chunk_size).min(bytes.len());
        let slice = &bytes[start..end];
        let is_last = chunk_id == total_chunks;

        let mut remaining_retries: i32 = CHUNK_UPLOAD_EXTRA_RETRIES as i32; // 2 -> 3 attempts total
        let mut logged_timeout_ms = CHUNK_UPLOAD_TIMEOUT_START_MS;

        loop {
            let req = UploadChunkRequest {
                bkid: &bkid,
                chunk_id,
                total_chunks,
                is_last,
                file_size,
                bytes: slice,
                logged_timeout_ms,
            };
            match transport.upload_chunk(&req).await {
                Ok(()) => {
                    state.last_uploaded_chunk_id = chunk_id;
                    let ui_pct =
                        compute_ui_upload_percent(chunk_id, total_chunks, cfg.backup_progress_weight);
                    on_ui_percent(ui_pct);
                    break;
                }
                Err(err) => {
                    let Some(kind) = err.retry_kind() else {
                        // Non-retryable error: fail immediately!
                        return Err(BackupPipelineError::Upload(err));
                    };
                    if remaining_retries <= 0 {
                        return Err(BackupPipelineError::Upload(err));
                    }
                    remaining_retries -= 1;

                    match kind {
                        ChunkRetryKind::Timeout => {
                            logged_timeout_ms = (logged_timeout_ms + CHUNK_UPLOAD_TIMEOUT_START_MS)
                                .min(CHUNK_UPLOAD_TIMEOUT_MAX_MS);
                            if timing.chunk_timeout_retry_delay_ms > 0 {
                                tokio::time::sleep(Duration::from_millis(
                                    timing.chunk_timeout_retry_delay_ms,
                                ))
                                .await;
                            }
                        }
                        ChunkRetryKind::Network => {
                            let back_online = wait_for_network_online(
                                transport,
                                timing.network_wait_max_ms,
                                timing.network_poll_interval_ms,
                            )
                            .await;
                            if !back_online {
                                return Err(BackupPipelineError::Upload(err));
                            }
                        }
                        ChunkRetryKind::ServerTransient => {
                            if timing.chunk_server_retry_delay_ms > 0 {
                                tokio::time::sleep(Duration::from_millis(
                                    timing.chunk_server_retry_delay_ms,
                                ))
                                .await;
                            }
                        }
                    }
                }
            }
        }
    }

    // Post-upload `hy` integrity verification.
    verify_post_upload_integrity(
        &file_path,
        file_size,
        chunk_size,
        total_chunks,
        state.last_uploaded_chunk_id,
        state.metadata.as_ref().map(|m| m.checksum.as_str()),
    )?;

    Ok(BackupUploadOutcome {
        bkid,
        file_size,
        total_chunks,
    })
}

async fn wait_for_network_online<T: BackupTransport>(
    transport: &T,
    max_wait_ms: u64,
    poll_interval_ms: u64,
) -> bool {
    if transport.is_online() {
        return true;
    }
    let step = poll_interval_ms.max(1);
    let mut waited = 0u64;
    while waited < max_wait_ms {
        let sleep_ms = step.min(max_wait_ms - waited);
        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        waited += sleep_ms;
        if transport.is_online() {
            return true;
        }
    }
    transport.is_online()
}

/// Exact port of post-upload `hy` integrity check in `main-startup`:
/// 1. `ceil(fileSize / chunk_size) == totalChunks`
/// 2. `lastUploadedChunkId == totalChunks`
/// 3. If metadata has non-empty `checksum`, recomputed SHA-256 of the file must match it.
pub fn verify_post_upload_integrity(
    file_path: &Path,
    file_size: u64,
    chunk_size: usize,
    total_chunks: u32,
    last_uploaded_chunk_id: u32,
    expected_checksum: Option<&str>,
) -> Result<(), BackupPipelineError> {
    let expected_chunks = ((file_size as usize).div_ceil(chunk_size.max(1))) as u32;
    if expected_chunks != total_chunks {
        return Err(BackupPipelineError::Integrity(format!(
            "chunk count mismatch: expected {expected_chunks}, got {total_chunks}"
        )));
    }
    if last_uploaded_chunk_id != total_chunks {
        return Err(BackupPipelineError::Integrity(format!(
            "last_uploaded_chunk_id {last_uploaded_chunk_id} != total_chunks {total_chunks}"
        )));
    }
    if let Some(exp) = expected_checksum.filter(|s| !s.is_empty()) {
        let (actual, actual_size) = compute_file_sha256_and_size(file_path)
            .map_err(|e| BackupPipelineError::Io(e.to_string()))?;
        if actual_size != file_size || actual != exp {
            return Err(BackupPipelineError::Integrity(format!(
                "post-upload checksum mismatch (expected {exp}, got {actual})"
            )));
        }
    }
    Ok(())
}

/// Exact port of `My()` (`triggerMobileRestore`) in `main-startup`:
/// ONLY called from:
/// - `onUserConfirmRestoreOnMobile` ("Use existing backup" button in `ExistingBackup` popup)
/// - `onUserStartMobileRestore` ("Continue on phone" button in `Success` banner)
/// ALWAYS swallows all errors (returns `true` if `633` returned `error_code == 0 && data.err == 0`,
/// `false` otherwise — never propagates an error to tear down the Success/AfterSuccess banner!).
pub async fn trigger_mobile_restore_swallow_errors<T: BackupTransport>(
    transport: &T,
    req_id: &str,
) -> bool {
    match transport.send_633_signal_restore(req_id).await {
        Ok(resp) => parse_633_response(&resp).is_ok(),
        Err(_) => false,
    }
}

/// QoS-only `backup_done` monitor (`armBackupDoneMonitor`).
/// NEVER controls whether the Success banner is shown — Success banner is ALWAYS shown first!
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupDoneMonitor {
    pub bkid: String,
    pub armed_at_ms: u64,
    pub timeout_ms: u64,
    pub status: BackupDoneMonitorStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupDoneMonitorStatus {
    Pending,
    MatchedOk,
    MatchedError(i64),
    MonitorTimeout,
}

impl BackupDoneMonitor {
    #[must_use]
    pub fn arm(bkid: String, now_ms: u64, cfg: &BackupRemoteConfig) -> Self {
        Self {
            bkid,
            armed_at_ms: now_ms,
            timeout_ms: cfg.backup_done_timeout_seconds.saturating_mul(1_000),
            status: BackupDoneMonitorStatus::Pending,
        }
    }

    /// Matches an incoming `backup_done` control event by `bkid`. Returns `true` if matched.
    pub fn on_backup_done(&mut self, bkid: &str, err: i64) -> bool {
        if self.status != BackupDoneMonitorStatus::Pending || self.bkid != bkid {
            return false;
        }
        self.status = if err == 0 {
            BackupDoneMonitorStatus::MatchedOk
        } else {
            BackupDoneMonitorStatus::MatchedError(err)
        };
        true
    }

    /// Checks whether the 180s QoS monitor timed out.
    pub fn check_timeout(&mut self, now_ms: u64) -> bool {
        if self.status != BackupDoneMonitorStatus::Pending {
            return false;
        }
        if now_ms.saturating_sub(self.armed_at_ms) >= self.timeout_ms {
            self.status = BackupDoneMonitorStatus::MonitorTimeout;
            return true;
        }
        false
    }
}

/// Persisted interrupt state (`z_backup_interrupted_` in `compact3197.js`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupInterruptState {
    pub bkid: Option<String>,
    pub file_path: Option<String>,
    pub interrupted_at_ms: u64,
    pub attempts: u32,
    pub last_uploaded_chunk_id: u32,
}

/// Resume step returned by `get_resume_step_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeStep {
    Engine,
    Upload,
}

/// Exact port of `getResumeStepFor`: if `file_path` OR `bkid` is present and non-empty,
/// resumes at `Upload`; otherwise resumes at `Engine`.
#[must_use]
pub fn get_resume_step_for(state: &BackupInterruptState) -> ResumeStep {
    let has_file = state.file_path.as_deref().is_some_and(|s| !s.is_empty());
    let has_bkid = state.bkid.as_deref().is_some_and(|s| !s.is_empty());
    if has_file || has_bkid {
        ResumeStep::Upload
    } else {
        ResumeStep::Engine
    }
}

/// Computes the next auto-resume backoff delay in ms, or `None` if the auto-resume window
/// (`auto_resume_window_seconds`, default 300s) expired or `attempts >= auto_resume_max_attempts` (default 3).
#[must_use]
pub fn next_auto_resume_delay_ms(
    state: &BackupInterruptState,
    now_ms: u64,
    cfg: &BackupRemoteConfig,
) -> Option<u64> {
    if state.attempts >= cfg.auto_resume_max_attempts {
        return None;
    }
    let window_ms = cfg.auto_resume_window_seconds.saturating_mul(1_000);
    if now_ms.saturating_sub(state.interrupted_at_ms) > window_ms {
        return None;
    }
    let idx = state.attempts as usize;
    cfg.auto_resume_backoff_ms
        .get(idx)
        .or_else(|| cfg.auto_resume_backoff_ms.last())
        .copied()
}
