//! Exact port of Electron's Sync 2 session layer (`class Uo`, `class Go`,
//! `class Zo`, `class Io`, `class zo` in `shared-worker.d6af5465eb38c618bfe0.js`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::{Notify, RwLock};

use crate::config::{TimeRange, TimeoutConfigs, JS_MAX_SAFE_INTEGER, SYNC_UNLIMITED_LIMIT};
use crate::proto::{
    read_framed_blocks, zstd_decompress, ConversationSyncChunk, MessageSyncChunk,
    SyncConversation, SyncMessage,
};
use crate::protocol::{
    generate_sync_id, BatchType, DeviceType, DisposeReason, MobileStatus, MobileStatusPayload,
    SyncDatabaseKind, SyncPriority, SyncRequest590, UploadBatchPayload, WireQuery,
};
use crate::wasm::{KeyPair, SyncWasm};

/// Transport abstraction for sending Sync 2 socket RPCs (`590`/`591`/`592`) and
/// downloading `msgUrl` batch files. Implemented by the desktop client and by
/// offline mock transports in unit tests.
pub trait SyncTransport: Send + Sync {
    /// Sends socket `590` (`REQUEST`) and returns `Ok(())` on `error_code == 0` ACK,
    /// or `Err(SyncSessionError)` on timeout / network / server error (e.g. `211`).
    fn send_request_590(
        &self,
        req: &SyncRequest590,
    ) -> impl std::future::Future<Output = Result<(), SyncSessionError>> + Send;

    /// Sends socket `591` (`ACK_DELETE_SYNC_SESSION`).
    fn send_delete_591(
        &self,
        sync_id: &str,
        to_device: DeviceType,
        reason: DisposeReason,
    ) -> impl std::future::Future<Output = Result<(), SyncSessionError>> + Send;

    /// Sends socket `592` (`REQUEST_MOBILE_WAKE_UP`), returning server status code (`0` = ok).
    fn send_wakeup_592(
        &self,
        sync_id: &str,
        to_device: DeviceType,
    ) -> impl std::future::Future<Output = Result<i64, SyncSessionError>> + Send;

    /// Downloads raw bytes of an uploaded encrypted batch file (`msgUrl`).
    fn download_batch(
        &self,
        url: &str,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, SyncSessionError>> + Send;
}

/// Errors produced by Sync 2 sessions, transport, or batch decoding.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SyncSessionError {
    /// Network disconnected or WebSocket not open.
    #[error("no network / socket not open")]
    NoNetwork,
    /// Socket `590` ACK timed out.
    #[error("request sync timeout")]
    RequestTimeout,
    /// Server rejected `590` with error code `211` (non-retryable).
    #[error("sync rejected by server (code 211)")]
    Rejected211,
    /// Waiting for phone confirmation timed out (`connectTimeout`).
    #[error("waiting for mobile confirmation timed out")]
    ConnectTimeout,
    /// Waiting for next batch timed out (`batchTimeout`).
    #[error("waiting for batch timed out")]
    BatchTimeout,
    /// Wakeup timed out (`wakeupTimeout`).
    #[error("mobile wakeup timed out")]
    WakeupTimeout,
    /// Session was released / cancelled while requests were pending.
    #[error("sync session released")]
    Released,
    /// Session was fulfilled without covering the requested scope.
    #[error("sync session fulfilled without matching scope")]
    NotEnough,
    /// Server or phone reported a non-zero error code on a batch or RPC.
    #[error("remote sync error code {code}: {message}")]
    Remote {
        /// Numeric error code.
        code: i64,
        /// Human-readable message.
        message: String,
    },
    /// Download / decrypt / decompress / protobuf decode failure.
    #[error("batch processing failed ({step}): {message}")]
    BatchDecode {
        /// Pipeline step (`Download`, `Decrypt`, `Decompress`, `Deserialize`).
        step: &'static str,
        /// Failure details.
        message: String,
    },
    /// Invalid internal state or argument.
    #[error("sync protocol invariant error: {0}")]
    Invalid(String),
}

/// Lifecycle step of a single `SyncSession` (`enum jo` in `shared-worker` line 7301).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum SessionStep {
    /// Initialized, `590` not yet ACKed.
    Init = 0,
    /// `590` ACK received (`start()` resolves here!).
    Requested = 1,
    /// Phone sent `Confirmed` (4) or `BypassConfirmed` (5).
    Preparing = 2,
    /// At least one batch has arrived.
    ReceivingBackup = 3,
    /// All batches `0..total_batch` have arrived.
    Fulfilled = 4,
    /// Session released (`591` sent if `already_start()`).
    Released = 5,
}

impl SessionStep {
    /// `alreadyStart()` (`currentStep > Init`).
    #[must_use]
    pub const fn already_start(self) -> bool {
        (self as u8) > (Self::Init as u8)
    }

    /// `isFulfilled()`.
    #[must_use]
    pub const fn is_fulfilled(self) -> bool {
        matches!(self, Self::Fulfilled)
    }

    /// `alreadyReleased()`.
    #[must_use]
    pub const fn already_released(self) -> bool {
        matches!(self, Self::Released)
    }

    /// Monotonic transition (`e > currentStep`).
    pub fn advance(&mut self, next: Self) {
        if (next as u8) > (*self as u8) {
            *self = next;
        }
    }
}

/// Session options derived from remote config (`Go.options`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionOptions {
    /// Connect/confirm timeout (ms). Drops to `0` once any session is confirmed.
    pub connect_timeout_ms: u64,
    /// Batch arrival timeout (ms).
    pub batch_timeout_ms: u64,
    /// Wakeup timeout (ms).
    pub wakeup_timeout_ms: u64,
    /// `req.batchSize` (rows per uploaded batch, fallback `2000`).
    pub transfer_batch_size: u64,
}

impl SessionOptions {
    /// Builds options from remote [`TimeoutConfigs`] and `transfer_batch_size`.
    #[must_use]
    pub const fn from_timeouts(timeouts: TimeoutConfigs, transfer_batch_size: u64) -> Self {
        Self {
            connect_timeout_ms: timeouts.confirm_timeout_ms,
            batch_timeout_ms: timeouts.batch_timeout_ms,
            wakeup_timeout_ms: timeouts.wakeup_timeout_ms,
            transfer_batch_size,
        }
    }
}

impl Default for SessionOptions {
    fn default() -> Self {
        // Default session manager options in `shared-worker` line 7486.
        Self {
            connect_timeout_ms: 10_000,
            batch_timeout_ms: 10_000,
            wakeup_timeout_ms: 180_000,
            transfer_batch_size: 2_000,
        }
    }
}

/// Internal request descriptor stored inside `SyncSession.requests` (`useSession` / `prepareSession`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionQuerySpec {
    /// Partition (`""` for conversations, `"oneone/..."` or `"group/..."` for messages).
    pub partition: String,
    /// Lower timestamp bound.
    pub from: u64,
    /// Upper timestamp bound.
    pub to: u64,
    /// Requested item limit.
    pub limit: u64,
    /// Database kind (`"conversation"` or `"message"`).
    pub db_kind: SyncDatabaseKind,
    /// Wire priority (`0` = High, `2` = Low).
    pub priority: SyncPriority,
}

/// One received batch scope recorded on `SyncSession.responsedScopes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponsedScope {
    /// `0` for success; non-zero on error.
    pub error_code: i64,
    /// Lower bound.
    pub from: u64,
    /// Upper bound (may be expanded by `expanse_scope_if_needed`).
    pub to: u64,
    /// Partition (`"metadata_{sync_id}"` for `MetadataEmbedded`).
    pub partition: String,
    /// Session id.
    pub sid: String,
    /// Batch index.
    pub idx: u64,
    /// True if final batch of session.
    pub is_last: bool,
    /// Remote download URL (`msgUrl`).
    pub file_path: String,
    /// Message or MetadataEmbedded.
    pub batch_type: BatchType,
}

/// Result of `SyncSession::get_sync_scope_info`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeMatch {
    /// Matched batch scope.
    pub scope: ResponsedScope,
    /// True if only the upper end `to` fell inside the batch (more older batches exist).
    pub has_more: bool,
}

/// `So(ts, scope)` (`shared-worker` line 7017): true if `ts` is within `[scope.from, scope.to]`.
#[must_use]
pub fn ts_in_scope(ts: u64, from: u64, to: u64) -> bool {
    let lo = from == 0 || ts >= from;
    let hi = to == 0 || ts <= to;
    lo && hi
}

/// `Mo(inner, outer)` (`shared-worker` line 7020): true if `inner` is contained in `outer`.
#[must_use]
pub fn range_contained(inner_from: u64, inner_to: u64, outer_from: u64, outer_to: u64) -> bool {
    let i_to = if inner_to == 0 {
        JS_MAX_SAFE_INTEGER
    } else {
        inner_to
    };
    let o_to = if outer_to == 0 {
        JS_MAX_SAFE_INTEGER
    } else {
        outer_to
    };
    inner_from >= outer_from && i_to <= o_to
}

/// Monotonic 0-based `ussidx` counter per `userSyncSessionId` (`class Lo` in `shared-worker`).
#[derive(Default)]
pub struct UssIdxCounter {
    map: Mutex<HashMap<String, u64>>,
}

impl UssIdxCounter {
    /// Returns the current index for `user_sync_session_id` (0 if new) and increments it.
    pub fn next_index(&self, user_sync_session_id: &str) -> u64 {
        let mut guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(user_sync_session_id.to_owned()).or_insert(0);
        let current = *entry;
        *entry = current.saturating_add(1);
        current
    }

    /// Resets the counter for `user_sync_session_id`.
    pub fn reset(&self, user_sync_session_id: &str) {
        let mut guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(user_sync_session_id);
    }
}

/// Active timer deadline tracked inside a [`SyncSession`].
#[derive(Clone, Debug)]
struct ActiveDeadline {
    at: Instant,
    err: SyncSessionError,
}

struct SessionInner {
    step: SessionStep,
    responsed_scopes: Vec<ResponsedScope>,
    total_batch: Option<u64>,
    responsed_batch_ids: HashSet<u64>,
    dedup_batch_info: HashSet<u64>,
    session_error: Option<SyncSessionError>,
    session_record: Option<Result<Vec<u8>, SyncSessionError>>,
    deadline: Option<ActiveDeadline>,
    meta_batch: Option<UploadBatchPayload>,
    meta_built: bool,
}

/// Single Sync 2 session (`class Uo` in `shared-worker.d6af5465eb38c618bfe0.js`).
pub struct SyncSession {
    sync_id: String,
    to_device: DeviceType,
    requests: Vec<SessionQuerySpec>,
    ik: KeyPair,
    ek: KeyPair,
    one_time_key: String,
    device_name: String,
    ussidx: u64,
    options: SessionOptions,
    inner: RwLock<SessionInner>,
    notify: Notify,
}

impl SyncSession {
    /// Creates a new session with fresh `syncId` and ephemeral key pair `ek`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        requests: Vec<SessionQuerySpec>,
        ik: KeyPair,
        ek: KeyPair,
        one_time_key: String,
        device_name: String,
        ussidx: u64,
        options: SessionOptions,
    ) -> Self {
        Self {
            sync_id: generate_sync_id(),
            to_device: DeviceType::Mobile,
            requests,
            ik,
            ek,
            one_time_key,
            device_name,
            ussidx,
            options,
            inner: RwLock::new(SessionInner {
                step: SessionStep::Init,
                responsed_scopes: Vec::new(),
                total_batch: None,
                responsed_batch_ids: HashSet::new(),
                dedup_batch_info: HashSet::new(),
                session_error: None,
                session_record: None,
                deadline: None,
                meta_batch: None,
                meta_built: false,
            }),
            notify: Notify::new(),
        }
    }

    /// Returns this session's `syncId`.
    #[must_use]
    pub fn sync_id(&self) -> &str {
        &self.sync_id
    }

    /// Returns the `590` payload that `start()` transmits on the wire.
    pub fn build_590(&self) -> Result<SyncRequest590, SyncSessionError> {
        if self.requests.is_empty() {
            return Err(SyncSessionError::Invalid(
                "cannot start sync session with 0 queries".into(),
            ));
        }
        let first_db = self.requests[0].db_kind;
        if self.requests.iter().any(|r| r.db_kind != first_db) {
            return Err(SyncSessionError::Invalid(
                "Permission denied, access multi database in one request.".into(),
            ));
        }
        let first_prio = self.requests[0].priority;
        if self.requests.iter().any(|r| r.priority != first_prio) {
            return Err(SyncSessionError::Invalid(
                "Permission denied, request multi priority in one request.".into(),
            ));
        }
        Ok(SyncRequest590 {
            sync_id: self.sync_id.clone(),
            sync_type: crate::protocol::SyncType::SyncFull,
            ek_public: self.ek.public_key.clone(),
            ik_public: self.ik.public_key.clone(),
            to_device: self.to_device,
            temp_key: self.one_time_key.clone(),
            device_name: self.device_name.clone(),
            db_kind: first_db,
            priority: first_prio,
            queries: self
                .requests
                .iter()
                .map(|r| WireQuery {
                    partition: r.partition.clone(),
                    from: r.from,
                    to: r.to,
                    limit: r.limit,
                })
                .collect(),
            batch_size: self.options.transfer_batch_size,
            ussidx: self.ussidx,
        })
    }

    /// Sends socket `590` and returns as soon as the server ACKs (`SessionStep::Requested`).
    /// Does NOT wait for any batch or `isLast`!
    pub async fn start<T: SyncTransport>(&self, transport: &T) -> Result<(), SyncSessionError> {
        {
            let inner = self.inner.read().await;
            if inner.step.already_released() {
                return Err(SyncSessionError::Released);
            }
        }
        let req = self.build_590()?;
        match transport.send_request_590(&req).await {
            Ok(()) => {
                let mut inner = self.inner.write().await;
                inner.step.advance(SessionStep::Requested);
                if self.options.connect_timeout_ms > 0 {
                    inner.deadline = Some(ActiveDeadline {
                        at: Instant::now() + Duration::from_millis(self.options.connect_timeout_ms),
                        err: SyncSessionError::ConnectTimeout,
                    });
                } else {
                    inner.deadline = Some(ActiveDeadline {
                        at: Instant::now() + Duration::from_millis(self.options.batch_timeout_ms),
                        err: SyncSessionError::BatchTimeout,
                    });
                }
                self.notify.notify_waiters();
                Ok(())
            }
            Err(err) => {
                let mut inner = self.inner.write().await;
                inner.session_error = Some(err.clone());
                self.notify.notify_waiters();
                Err(err)
            }
        }
    }

    /// Releases this session (`Uo.release(reason)`): sends `591` ONLY if `already_start()`,
    /// marks step `Released`, and rejects any waiting scope queries.
    pub async fn release<T: SyncTransport>(
        &self,
        transport: &T,
        reason: DisposeReason,
    ) -> Result<(), SyncSessionError> {
        let should_send_591 = {
            let mut inner = self.inner.write().await;
            let started = inner.step.already_start() && !inner.step.already_released();
            inner.step.advance(SessionStep::Released);
            inner.deadline = None;
            inner.responsed_scopes.clear();
            inner.responsed_batch_ids.clear();
            inner.dedup_batch_info.clear();
            inner.session_error = Some(SyncSessionError::Released);
            self.notify.notify_waiters();
            started
        };
        if should_send_591 {
            let _ = transport
                .send_delete_591(&self.sync_id, self.to_device, reason)
                .await;
        }
        Ok(())
    }

    /// Sends socket `592` (`wakeup`) and arms `wakeupTimeout` when code == 0.
    pub async fn wakeup<T: SyncTransport>(&self, transport: &T) -> Result<i64, SyncSessionError> {
        let code = transport
            .send_wakeup_592(&self.sync_id, self.to_device)
            .await?;
        if code == 0 {
            let mut inner = self.inner.write().await;
            if !inner.step.is_fulfilled() && !inner.step.already_released() {
                inner.deadline = Some(ActiveDeadline {
                    at: Instant::now() + Duration::from_millis(self.options.wakeup_timeout_ms),
                    err: SyncSessionError::WakeupTimeout,
                });
                self.notify.notify_waiters();
            }
        }
        Ok(code)
    }

    /// True once step >= `Preparing` (`Confirmed` / `BypassConfirmed` / receiving).
    pub async fn already_confirmed(&self) -> bool {
        let inner = self.inner.read().await;
        (inner.step as u8) >= (SessionStep::Preparing as u8)
    }

    /// True if all batches have arrived.
    pub async fn is_fulfilled(&self) -> bool {
        self.inner.read().await.step.is_fulfilled()
    }

    /// Handles a `transfer_status` control event matching this session's `syncId` (`Uo.onConnectionChanged`).
    pub async fn on_mobile_status(&self, payload: &MobileStatusPayload) {
        if payload.sync_id != self.sync_id {
            return;
        }
        let mut inner = self.inner.write().await;
        if inner.step.is_fulfilled() || inner.step.already_released() {
            return;
        }
        match payload.status {
            MobileStatus::Active => {
                inner.deadline = Some(ActiveDeadline {
                    at: Instant::now() + Duration::from_millis(self.options.batch_timeout_ms),
                    err: SyncSessionError::BatchTimeout,
                });
            }
            MobileStatus::Confirmed | MobileStatus::BypassConfirmed => {
                inner.step.advance(SessionStep::Preparing);
                inner.deadline = Some(ActiveDeadline {
                    at: Instant::now() + Duration::from_millis(self.options.batch_timeout_ms),
                    err: SyncSessionError::BatchTimeout,
                });
            }
            MobileStatus::WaitingConfirm => {
                inner.deadline = Some(ActiveDeadline {
                    at: Instant::now() + Duration::from_millis(self.options.connect_timeout_ms),
                    err: SyncSessionError::ConnectTimeout,
                });
            }
            MobileStatus::Idle => {
                inner.deadline = None;
            }
            _ => {}
        }
        self.notify.notify_waiters();
    }

    /// Handles an incoming `upload_batch` or `transfer_error` event (`Uo.handleResponse`).
    pub async fn on_batch(&self, mut payload: UploadBatchPayload) {
        if payload.sync_id != self.sync_id {
            return;
        }
        let mut inner = self.inner.write().await;
        if inner.step.is_fulfilled() || inner.step.already_released() {
            return;
        }
        if payload.batch_type == BatchType::MetadataEmbedded {
            for s in &mut payload.scopes {
                s.partition = format!("metadata_{}", payload.sync_id);
            }
            if payload.err != 0 {
                let err = SyncSessionError::Remote {
                    code: payload.err,
                    message: format!("Receive meta batch error {}", payload.err),
                };
                inner.session_record = Some(Err(err.clone()));
                inner.session_error = Some(err);
            } else {
                inner.meta_batch = Some(payload.clone());
            }
        }

        inner.step.advance(SessionStep::ReceivingBackup);
        if !inner.dedup_batch_info.insert(payload.idx) {
            return;
        }

        inner.responsed_batch_ids.insert(payload.idx);
        if payload.is_last {
            inner.total_batch = Some(payload.idx + 1);
        }
        if let Some(total) = inner.total_batch {
            if inner.responsed_batch_ids.len() as u64 == total {
                inner.step.advance(SessionStep::Fulfilled);
                inner.deadline = None;
            }
        }

        let mut expanded_first = false;
        for s in &payload.scopes {
            let scope = ResponsedScope {
                error_code: payload.err,
                from: s.from,
                to: if s.to == 0 { JS_MAX_SAFE_INTEGER } else { s.to },
                partition: s.partition.clone(),
                sid: payload.sync_id.clone(),
                idx: payload.idx,
                is_last: payload.is_last,
                file_path: payload.msg_url.clone(),
                batch_type: payload.batch_type,
            };
            let new_idx = inner.responsed_scopes.len();
            inner.responsed_scopes.push(scope);
            if !expanded_first {
                expanded_first = true;
                expanse_scope_if_needed(&mut inner.responsed_scopes, new_idx);
            }
        }

        if !inner.step.is_fulfilled() {
            inner.deadline = Some(ActiveDeadline {
                at: Instant::now() + Duration::from_millis(self.options.batch_timeout_ms),
                err: SyncSessionError::BatchTimeout,
            });
        }
        self.notify.notify_waiters();
    }

    /// Exact port of `Uo.canReuseThisSession(query)` (`shared-worker` line 7352).
    pub async fn can_reuse(&self, query: &SessionQuerySpec) -> bool {
        let inner = self.inner.read().await;
        let has_err = inner.session_error.is_some();
        let existing = find_matching_scope(query, &inner.responsed_scopes);
        let scope_ok = existing
            .as_ref()
            .is_some_and(|m| m.scope.error_code == 0);
        let scope_err = existing
            .as_ref()
            .is_some_and(|m| m.scope.error_code != 0);

        self.requests.iter().any(|req| {
            let geom_ok = query.partition == req.partition
                && query.limit <= req.limit
                && range_contained(query.from, query.to, req.from, req.to)
                && (req.limit == SYNC_UNLIMITED_LIMIT || query.to == req.to);
            let prio_ok = (query.priority as u8) <= (req.priority as u8);
            (geom_ok && scope_ok) || (!scope_err && !has_err && geom_ok && prio_ok)
        })
    }

    /// Waits until a batch covering `query` arrives, or fails on timeout/error/fulfilled (`Uo.getSyncScopeInfo`).
    pub async fn wait_scope_info(
        &self,
        query: &SessionQuerySpec,
    ) -> Result<ScopeMatch, SyncSessionError> {
        if !self.can_reuse(query).await {
            return Err(SyncSessionError::Invalid(
                "Get encrypt info before make sync request".into(),
            ));
        }
        loop {
            let notified = self.notify.notified();
            let deadline_opt = {
                let inner = self.inner.read().await;
                if let Some(found) = find_matching_scope(query, &inner.responsed_scopes) {
                    return Ok(found);
                }
                if inner.step.is_fulfilled() {
                    return Err(SyncSessionError::NotEnough);
                }
                if let Some(err) = &inner.session_error {
                    return Err(err.clone());
                }
                inner.deadline.clone()
            };

            if let Some(dl) = deadline_opt {
                let now = Instant::now();
                if now >= dl.at {
                    let mut inner = self.inner.write().await;
                    if !inner.step.is_fulfilled() && inner.session_error.is_none() {
                        inner.session_error = Some(dl.err.clone());
                        self.notify.notify_waiters();
                    }
                    return Err(dl.err);
                }
                let sleep = tokio::time::sleep(dl.at - now);
                tokio::select! {
                    () = notified => {}
                    () = sleep => {
                        let mut inner = self.inner.write().await;
                        if let Some(cur) = &inner.deadline {
                            if Instant::now() >= cur.at && !inner.step.is_fulfilled() {
                                let err = cur.err.clone();
                                inner.session_error = Some(err.clone());
                                self.notify.notify_waiters();
                                return Err(err);
                            }
                        }
                    }
                }
            } else {
                notified.await;
            }
        }
    }

    /// Refreshes this session's `BatchTimeout` deadline if it is still queued (`Requested`
    /// or `Preparing`) waiting for its turn behind another active session in the same manager.
    pub async fn refresh_queued_batch_deadline(&self) {
        let mut inner = self.inner.write().await;
        if matches!(inner.step, SessionStep::Requested | SessionStep::Preparing)
            && inner.session_error.is_none()
        {
            if let Some(ref mut dl) = inner.deadline {
                if dl.err == SyncSessionError::BatchTimeout {
                    dl.at = Instant::now() + Duration::from_millis(self.options.batch_timeout_ms);
                    self.notify.notify_waiters();
                }
            }
        }
    }

    /// Ensures the `MetadataEmbedded` batch (if present) has been downloaded & decrypted
    /// so `session_record` is ready before decrypting `Message` batches (`Zo.buildMetadata`).
    pub async fn ensure_metadata_built<T: SyncTransport>(
        &self,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<(), SyncSessionError> {
        let meta = {
            let mut inner = self.inner.write().await;
            if inner.meta_built || inner.session_record.is_some() {
                return Ok(());
            }
            let Some(m) = inner.meta_batch.clone() else {
                return Ok(());
            };
            inner.meta_built = true;
            m
        };
        let bytes = match transport.download_batch(&meta.msg_url).await {
            Ok(b) => b,
            Err(err) => {
                let mut inner = self.inner.write().await;
                inner.meta_built = false;
                self.notify.notify_waiters();
                return Err(err);
            }
        };
        let blocks = match read_framed_blocks(&bytes) {
            Ok(b) => b,
            Err(e) => {
                let mut inner = self.inner.write().await;
                inner.meta_built = false;
                self.notify.notify_waiters();
                return Err(SyncSessionError::BatchDecode {
                    step: "Download",
                    message: e.to_string(),
                });
            }
        };
        for block in blocks {
            let _ = self.decrypt_metadata_block(wasm, &block).await?;
        }
        Ok(())
    }

    async fn decrypt_metadata_block(
        &self,
        wasm: &Mutex<SyncWasm>,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, SyncSessionError> {
        let res = {
            let mut guard = wasm.lock().unwrap_or_else(|e| e.into_inner());
            guard.decrypt_metadata(
                ciphertext,
                &self.ik.public_key,
                &self.ik.private_key,
                &self.ek.private_key,
                None,
            )
        };
        match res {
            Ok(dec) => {
                let mut inner = self.inner.write().await;
                inner.session_record = Some(Ok(dec.session_record));
                self.notify.notify_waiters();
                Ok(dec.plaintext)
            }
            Err(e) => {
                let err = SyncSessionError::BatchDecode {
                    step: "Decrypt",
                    message: e.to_string(),
                };
                let mut inner = self.inner.write().await;
                inner.session_record = Some(Err(err.clone()));
                inner.session_error = Some(err.clone());
                self.notify.notify_waiters();
                Err(err)
            }
        }
    }

    /// Decrypts a single block (`Uo.decryptChunk`).
    pub async fn decrypt_chunk<T: SyncTransport>(
        &self,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
        ciphertext: &[u8],
        batch_type: BatchType,
    ) -> Result<Vec<u8>, SyncSessionError> {
        match batch_type {
            BatchType::MetadataEmbedded => self.decrypt_metadata_block(wasm, ciphertext).await,
            BatchType::Message => {
                let session_record = self.wait_session_record(transport, wasm).await?;
                let res = {
                    let mut guard = wasm.lock().unwrap_or_else(|e| e.into_inner());
                    guard.decrypt_message(ciphertext, &session_record)
                };
                res.map(|d| d.plaintext)
                    .map_err(|e| SyncSessionError::BatchDecode {
                        step: "Decrypt",
                        message: e.to_string(),
                    })
            }
        }
    }

    /// Waits up to 60 seconds for `session_record` from `MetadataEmbedded` (`Uo.waitSessionRecord`),
    /// dynamically triggering [`Self::ensure_metadata_built`] if `MetadataEmbedded` arrives after
    /// the first `Message` batch (matching Electron's `addWaitingBuildMeta`/`addReadyBuildMeta`).
    async fn wait_session_record<T: SyncTransport>(
        &self,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<Vec<u8>, SyncSessionError> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let notified = self.notify.notified();
            let should_build_meta = {
                let inner = self.inner.read().await;
                if let Some(res) = &inner.session_record {
                    return res.clone();
                }
                if let Some(err) = &inner.session_error {
                    return Err(err.clone());
                }
                inner.meta_batch.is_some() && !inner.meta_built
            };
            if should_build_meta {
                self.ensure_metadata_built(transport, wasm).await?;
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(SyncSessionError::BatchDecode {
                    step: "Decrypt",
                    message: "Timeout session record!".into(),
                });
            }
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep(deadline - now) => {
                    return Err(SyncSessionError::BatchDecode {
                        step: "Decrypt",
                        message: "Timeout session record!".into(),
                    });
                }
            }
        }
    }
}

/// Exact port of `Uo.expanseScopeIfNeeded` (`shared-worker` line 7431).
fn expanse_scope_if_needed(scopes: &mut [ResponsedScope], new_idx: usize) {
    let idx = scopes[new_idx].idx;
    let to = scopes[new_idx].to;
    if idx == 0 || to == 0 {
        return;
    }
    let partition = scopes[new_idx].partition.clone();
    let new_from = scopes[new_idx].from;
    let mut prev_from: Option<u64> = None;
    let mut next_pos: Option<usize> = None;
    for (i, s) in scopes.iter().enumerate() {
        if i == new_idx || s.partition != partition {
            continue;
        }
        if s.idx + 1 == idx {
            prev_from = Some(s.from);
        } else if s.idx == idx + 1 {
            next_pos = Some(i);
        }
        if prev_from.is_some() && next_pos.is_some() {
            break;
        }
    }
    if let Some(pf) = prev_from {
        scopes[new_idx].to = pf.saturating_sub(1);
    }
    if let Some(np) = next_pos {
        scopes[np].to = new_from.saturating_sub(1);
    }
}

/// Exact port of `Uo.doGetEncryptInfo` (`shared-worker` line 7385).
fn find_matching_scope(query: &SessionQuerySpec, scopes: &[ResponsedScope]) -> Option<ScopeMatch> {
    if let Some(full) = scopes
        .iter()
        .find(|s| s.partition == query.partition && range_contained(query.from, query.to, s.from, s.to))
    {
        return Some(ScopeMatch {
            scope: full.clone(),
            has_more: false,
        });
    }
    scopes
        .iter()
        .find(|s| s.partition == query.partition && ts_in_scope(query.to, s.from, s.to))
        .map(|part| ScopeMatch {
            scope: part.clone(),
            has_more: true,
        })
}

/// Groups prepare queries by priority (descending) into batches of `< 30` (i.e. up to 30
/// queries per `PrepareAll` session), matching `Zo.buildPrepareQuery` (`shared-worker` line 7702).
#[must_use]
pub fn group_prepare_queries(queries: Vec<SessionQuerySpec>) -> Vec<Vec<SessionQuerySpec>> {
    let mut by_prio: HashMap<u8, Vec<Vec<SessionQuerySpec>>> = HashMap::new();
    for q in queries {
        let groups = by_prio.entry(q.priority as u8).or_default();
        if let Some(slot) = groups.iter_mut().find(|g| g.len() < 30) {
            slot.push(q);
        } else {
            groups.push(vec![q]);
        }
    }
    let mut prios: Vec<u8> = by_prio.keys().copied().collect();
    prios.sort_unstable_by(|a, b| b.cmp(a));
    let mut out = Vec::new();
    for p in prios {
        if let Some(groups) = by_prio.remove(&p) {
            out.extend(groups);
        }
    }
    out
}

#[derive(Clone)]
enum UrlLoadState {
    Loading(Arc<Notify>),
    Loaded,
}

/// Manages active [`SyncSession`]s for one remote database (`"conversation"` or `"message"`),
/// mirroring `class Go` (`SessionManager`) + `class Zo` (`DataBuilder`) in `shared-worker`.
pub struct SessionManager {
    ik: KeyPair,
    one_time_key: String,
    user_sync_session_id: String,
    device_name: String,
    options: SessionOptions,
    ussidx: Arc<UssIdxCounter>,
    sessions: RwLock<Vec<Arc<SyncSession>>>,
    first_request_at: Mutex<Option<Instant>>,
    /// In-flight and completed URL loads so concurrent GetAll tasks sharing the same
    /// batch file URL await the single unpack instead of reading an empty partition cache
    /// (mirrors Electron's `pager-cache`).
    url_loads: RwLock<HashMap<String, UrlLoadState>>,
}

impl SessionManager {
    /// Creates a new manager for one database (`conversation` or `message`).
    pub fn new(
        ik: KeyPair,
        one_time_key: String,
        user_sync_session_id: String,
        device_name: String,
        options: SessionOptions,
        ussidx: Arc<UssIdxCounter>,
    ) -> Self {
        Self {
            ik,
            one_time_key,
            user_sync_session_id,
            device_name,
            options,
            ussidx,
            sessions: RwLock::new(Vec::new()),
            first_request_at: Mutex::new(None),
            url_loads: RwLock::new(HashMap::new()),
        }
    }

    /// Computes effective options for the next session (`Go.syncOptions`, line 7488):
    /// `connectTimeout = 0` once any session has confirmed, else `max(0, configured - elapsed)`.
    pub async fn effective_options(&self) -> SessionOptions {
        let sessions = self.sessions.read().await;
        let mut any_confirmed = false;
        for s in sessions.iter() {
            if s.already_confirmed().await {
                any_confirmed = true;
                break;
            }
        }
        let elapsed_ms = self
            .first_request_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map_or(0, |t| t.elapsed().as_millis() as u64);
        SessionOptions {
            connect_timeout_ms: if any_confirmed {
                0
            } else {
                self.options.connect_timeout_ms.saturating_sub(elapsed_ms)
            },
            ..self.options
        }
    }

    /// Creates and starts a new session (`Go.create`).
    ///
    /// - First session sends `one_time_key`; subsequent sessions send `""`.
    /// - Awaits ONLY `start()` (ACK of `590`), never waiting for `isLast` or releasing previous sessions.
    pub async fn create<T: SyncTransport>(
        &self,
        queries: Vec<SessionQuerySpec>,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<Arc<SyncSession>, SyncSessionError> {
        let ek = {
            let mut guard = wasm.lock().unwrap_or_else(|e| e.into_inner());
            guard.gen_keypair().map_err(|e| SyncSessionError::BatchDecode {
                step: "Decrypt",
                message: format!("genSyncKeyPair: {e}"),
            })?
        };
        let opts = self.effective_options().await;
        let idx = self.ussidx.next_index(&self.user_sync_session_id);
        let session = {
            let mut list = self.sessions.write().await;
            // Read `list.is_empty()` BEFORE push (`syncSessions.length ? "" : oneTimeKey`).
            let temp_key = if list.is_empty() {
                self.one_time_key.clone()
            } else {
                String::new()
            };
            let s = Arc::new(SyncSession::new(
                queries,
                self.ik.clone(),
                ek,
                temp_key,
                self.device_name.clone(),
                idx,
                opts,
            ));
            list.push(Arc::clone(&s));
            s
        };
        {
            let mut first = self
                .first_request_at
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if first.is_none() {
                *first = Some(Instant::now());
            }
        }
        if let Err(err) = session.start(transport).await {
            let mut list = self.sessions.write().await;
            list.retain(|s| s.sync_id() != session.sync_id());
            return Err(err);
        }
        Ok(session)
    }

    /// Reuses an existing session covering `query` if available, else opens a 1-query session (`Go.useSession`).
    pub async fn use_session<T: SyncTransport>(
        &self,
        query: SessionQuerySpec,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<Arc<SyncSession>, SyncSessionError> {
        if let Some(existing) = self.find_reusable(&query).await {
            return Ok(existing);
        }
        self.create(vec![query], transport, wasm).await
    }

    /// Prepares a group of queries (`Go.prepareSession`): filters out any already covered by
    /// an existing reusable session, then opens ONE session for the remaining queries.
    pub async fn prepare_session<T: SyncTransport>(
        &self,
        queries: Vec<SessionQuerySpec>,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<Option<Arc<SyncSession>>, SyncSessionError> {
        let mut needed = Vec::new();
        for q in queries {
            if self.find_reusable(&q).await.is_none() {
                needed.push(q);
            }
        }
        if needed.is_empty() {
            return Ok(None);
        }
        let s = self.create(needed, transport, wasm).await?;
        Ok(Some(s))
    }

    /// Runs `buildPrepareQuery` over all queries (grouping `< 30` per priority, awaiting each
    /// group's `590` ACK in order without releasing previous sessions).
    pub async fn prepare_all<T: SyncTransport>(
        &self,
        queries: Vec<SessionQuerySpec>,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<usize, SyncSessionError> {
        let groups = group_prepare_queries(queries);
        let mut prepared = 0usize;
        for group in groups {
            let count = group.len();
            let _ = self.prepare_session(group, transport, wasm).await?;
            prepared += count;
        }
        Ok(prepared)
    }

    /// Finds the first existing session that satisfies `can_reuse(query)`.
    pub async fn find_reusable(&self, query: &SessionQuerySpec) -> Option<Arc<SyncSession>> {
        let list = self.sessions.read().await.clone();
        for s in list {
            if s.can_reuse(query).await {
                return Some(s);
            }
        }
        None
    }

    /// Returns whether any session in this manager has `sync_id`.
    pub async fn has_session(&self, sync_id: &str) -> bool {
        self.sessions
            .read()
            .await
            .iter()
            .any(|s| s.sync_id() == sync_id)
    }

    /// Routes an incoming `transfer_status` to the matching session.
    /// When the phone reports `Active` on any session, refreshes queued prepared sessions'
    /// `BatchTimeout` deadlines so sessions waiting their turn on mobile do not falsely expire.
    pub async fn dispatch_mobile_status(&self, payload: &MobileStatusPayload) -> bool {
        let list = self.sessions.read().await.clone();
        let mut matched = false;
        for s in &list {
            if s.sync_id() == payload.sync_id {
                s.on_mobile_status(payload).await;
                matched = true;
                break;
            }
        }
        if matched
            && matches!(
                payload.status,
                MobileStatus::Active | MobileStatus::Confirmed | MobileStatus::BypassConfirmed
            )
        {
            for s in &list {
                if s.sync_id() != payload.sync_id {
                    s.refresh_queued_batch_deadline().await;
                }
            }
        }
        matched
    }

    /// Routes an incoming `upload_batch` or `transfer_error` to the matching session,
    /// and refreshes `BatchTimeout` on other queued prepared sessions while mobile is actively uploading.
    pub async fn dispatch_batch(&self, payload: UploadBatchPayload) -> bool {
        let sid = payload.sync_id.clone();
        let list = self.sessions.read().await.clone();
        let mut matched = false;
        for s in &list {
            if s.sync_id() == sid {
                s.on_batch(payload).await;
                matched = true;
                break;
            }
        }
        if matched {
            for s in &list {
                if s.sync_id() != sid {
                    s.refresh_queued_batch_deadline().await;
                }
            }
        }
        matched
    }

    /// Sends wakeup `592` on the first active session (`Go.wakeup`).
    pub async fn wakeup<T: SyncTransport>(&self, transport: &T) -> Result<i64, SyncSessionError> {
        let first = self.sessions.read().await.first().cloned();
        match first {
            Some(s) => s.wakeup(transport).await,
            None => Ok(-1),
        }
    }

    /// Releases every open session with `reason` (`Go.release`).
    pub async fn release_all<T: SyncTransport>(&self, transport: &T, reason: DisposeReason) {
        let list = {
            let mut guard = self.sessions.write().await;
            std::mem::take(&mut *guard)
        };
        for s in list {
            let _ = s.release(transport, reason).await;
        }
        let prev = {
            let mut loads = self.url_loads.write().await;
            std::mem::take(&mut *loads)
        };
        for (_, state) in prev {
            if let UrlLoadState::Loading(n) = state {
                n.notify_waiters();
            }
        }
    }

    /// Claims ownership of downloading & unpacking `url`.
    ///
    /// - Returns `true` if the caller must download & unpack `url` and then call
    ///   [`Self::finish_url_load`].
    /// - If another task is currently downloading `url`, **awaits** until that task finishes
    ///   (so callers never read an empty partition cache!), then returns `false` if loaded
    ///   (or retries claiming if the previous attempt failed).
    pub async fn begin_url_load(&self, url: &str) -> bool {
        loop {
            let wait_notify = {
                let mut loads = self.url_loads.write().await;
                match loads.get(url) {
                    Some(UrlLoadState::Loaded) => return false,
                    Some(UrlLoadState::Loading(n)) => Arc::clone(n),
                    None => {
                        loads.insert(url.to_owned(), UrlLoadState::Loading(Arc::new(Notify::new())));
                        return true;
                    }
                }
            };
            wait_notify.notified().await;
        }
    }

    /// Completes an in-flight [`Self::begin_url_load`] claim and wakes all tasks waiting on `url`.
    pub async fn finish_url_load(&self, url: &str, ok: bool) {
        let notify = {
            let mut loads = self.url_loads.write().await;
            let prev = if ok {
                loads.insert(url.to_owned(), UrlLoadState::Loaded)
            } else {
                loads.remove(url)
            };
            match prev {
                Some(UrlLoadState::Loading(n)) => Some(n),
                _ => None,
            }
        };
        if let Some(n) = notify {
            n.notify_waiters();
        }
    }
}

/// In-memory remote database cache + `executeGetAllQuery` loop (`zo` + `Io` in `shared-worker`)
/// for conversation batches.
pub struct ConversationRemoteDb {
    manager: SessionManager,
    /// Descending-sorted conversation rows indexed by `last_ts`.
    rows: RwLock<Vec<SyncConversation>>,
}

impl ConversationRemoteDb {
    /// Creates an open conversation remote database.
    pub fn new(manager: SessionManager) -> Self {
        Self {
            manager,
            rows: RwLock::new(Vec::new()),
        }
    }

    /// Accesses the underlying [`SessionManager`] for dispatching control events / closing.
    #[must_use]
    pub fn manager(&self) -> &SessionManager {
        &self.manager
    }

    /// Executes `PrepareAll` for conversations (`prepareSyncConversations`).
    pub async fn prepare<T: SyncTransport>(
        &self,
        range: TimeRange,
        limit: i64,
        priority: SyncPriority,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<(), SyncSessionError> {
        let q = SessionQuerySpec {
            partition: String::new(),
            from: range.from_ts,
            to: range.to_ts,
            limit: if limit > 0 {
                limit as u64
            } else {
                SYNC_UNLIMITED_LIMIT
            },
            db_kind: SyncDatabaseKind::Conversation,
            priority,
        };
        let _ = self.manager.prepare_all(vec![q], transport, wasm).await?;
        Ok(())
    }

    /// Executes one `GetAll` conversation query, iterating internally with `to = last_ts - 1`
    /// and `limit = remaining` while `scope_match.has_more` (`zo.executeGetAllQuery`, line 7227).
    pub async fn execute_get_all<T: SyncTransport>(
        &self,
        range: TimeRange,
        limit: u64,
        priority: SyncPriority,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<Vec<SyncConversation>, SyncSessionError> {
        let mut collected: Vec<SyncConversation> = Vec::new();
        let mut cur_to = range.to_ts;
        let effective_limit = if limit > 0 { limit } else { SYNC_UNLIMITED_LIMIT };

        loop {
            let remaining = effective_limit.saturating_sub(collected.len() as u64);
            if remaining == 0 {
                break;
            }
            let q = SessionQuerySpec {
                partition: String::new(),
                from: range.from_ts,
                to: cur_to,
                limit: remaining,
                db_kind: SyncDatabaseKind::Conversation,
                priority,
            };
            let session = self.manager.use_session(q.clone(), transport, wasm).await?;
            let scope_match = session.wait_scope_info(&q).await?;
            if scope_match.scope.error_code != 0 {
                return Err(SyncSessionError::Remote {
                    code: scope_match.scope.error_code,
                    message: "Error in external system".into(),
                });
            }
            session.ensure_metadata_built(transport, wasm).await?;
            let url = &scope_match.scope.file_path;
            if self.manager.begin_url_load(url).await {
                let load_res: Result<Vec<SyncConversation>, SyncSessionError> = async {
                    let raw = transport.download_batch(url).await?;
                    let blocks =
                        read_framed_blocks(&raw).map_err(|e| SyncSessionError::BatchDecode {
                            step: "Download",
                            message: e.to_string(),
                        })?;
                    let mut unpacked = Vec::new();
                    for block in blocks {
                        let dec = session
                            .decrypt_chunk(transport, wasm, &block, scope_match.scope.batch_type)
                            .await?;
                        let plain =
                            zstd_decompress(&dec).map_err(|e| SyncSessionError::BatchDecode {
                                step: "Decompress",
                                message: e.to_string(),
                            })?;
                        let chunk = ConversationSyncChunk::decode(&plain).map_err(|e| {
                            SyncSessionError::BatchDecode {
                                step: "Deserialize",
                                message: e.to_string(),
                            }
                        })?;
                        unpacked.extend(chunk.conversations);
                    }
                    Ok(unpacked)
                }
                .await;
                match load_res {
                    Ok(unpacked) => {
                        {
                            let mut rows = self.rows.write().await;
                            rows.extend(unpacked);
                            rows.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
                        }
                        self.manager.finish_url_load(url, true).await;
                    }
                    Err(err) => {
                        self.manager.finish_url_load(url, false).await;
                        return Err(err);
                    }
                }
            }

            let batch_rows: Vec<SyncConversation> = {
                let rows = self.rows.read().await;
                rows.iter()
                    .filter(|c| ts_in_scope(c.last_ts, range.from_ts, cur_to))
                    .take(remaining as usize)
                    .cloned()
                    .collect()
            };
            collected.extend(batch_rows);

            if !scope_match.has_more || collected.len() as u64 >= effective_limit {
                break;
            }
            if let Some(last) = collected.last() {
                if last.last_ts == 0 {
                    break;
                }
                cur_to = last.last_ts.saturating_sub(1);
            } else {
                break;
            }
        }
        Ok(collected)
    }

    /// Closes the conversation database ONCE with `reason` (e.g., `DisposeReason::SyncSuccess`).
    pub async fn close<T: SyncTransport>(&self, transport: &T, reason: DisposeReason) {
        self.manager.release_all(transport, reason).await;
        self.rows.write().await.clear();
    }
}

/// In-memory remote database cache + `executeGetAllQuery` loop for message batches.
pub struct MessageRemoteDb {
    manager: SessionManager,
    /// Per-partition messages sorted descending by `timestamp`.
    partitions: RwLock<HashMap<String, Vec<SyncMessage>>>,
}

impl MessageRemoteDb {
    /// Creates an open message remote database.
    pub fn new(manager: SessionManager) -> Self {
        Self {
            manager,
            partitions: RwLock::new(HashMap::new()),
        }
    }

    /// Accesses the underlying [`SessionManager`].
    #[must_use]
    pub fn manager(&self) -> &SessionManager {
        &self.manager
    }

    /// Executes `PrepareAll` grouped `< 30` queries per priority (`prepareSyncOtherMessages` /
    /// `prepareSyncPreviewAndRecentMessages`).
    pub async fn prepare_messages<T: SyncTransport>(
        &self,
        queries: Vec<SessionQuerySpec>,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<usize, SyncSessionError> {
        self.manager.prepare_all(queries, transport, wasm).await
    }

    /// Executes a single-conversation `GetAll` message query (`executeMessageQuery`).
    pub async fn execute_get_all<T: SyncTransport>(
        &self,
        partition: &str,
        range: TimeRange,
        limit: u64,
        priority: SyncPriority,
        transport: &T,
        wasm: &Mutex<SyncWasm>,
    ) -> Result<Vec<SyncMessage>, SyncSessionError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut collected: Vec<SyncMessage> = Vec::new();
        let mut cur_to = range.to_ts;

        loop {
            let remaining = limit.saturating_sub(collected.len() as u64);
            if remaining == 0 {
                break;
            }
            let q = SessionQuerySpec {
                partition: partition.to_owned(),
                from: range.from_ts,
                to: cur_to,
                limit: remaining,
                db_kind: SyncDatabaseKind::Message,
                priority,
            };
            let session = self.manager.use_session(q.clone(), transport, wasm).await?;
            let scope_match = session.wait_scope_info(&q).await?;
            if scope_match.scope.error_code != 0 {
                return Err(SyncSessionError::Remote {
                    code: scope_match.scope.error_code,
                    message: "Error in external system".into(),
                });
            }
            session.ensure_metadata_built(transport, wasm).await?;
            let url = &scope_match.scope.file_path;
            if self.manager.begin_url_load(url).await {
                let load_res: Result<Vec<(String, Vec<SyncMessage>)>, SyncSessionError> = async {
                    let raw = transport.download_batch(url).await?;
                    let blocks =
                        read_framed_blocks(&raw).map_err(|e| SyncSessionError::BatchDecode {
                            step: "Download",
                            message: e.to_string(),
                        })?;
                    let mut decoded_parts = Vec::new();
                    for block in blocks {
                        let dec = session
                            .decrypt_chunk(transport, wasm, &block, scope_match.scope.batch_type)
                            .await?;
                        let plain =
                            zstd_decompress(&dec).map_err(|e| SyncSessionError::BatchDecode {
                                step: "Decompress",
                                message: e.to_string(),
                            })?;
                        let chunk = MessageSyncChunk::decode(&plain).map_err(|e| {
                            SyncSessionError::BatchDecode {
                                step: "Deserialize",
                                message: e.to_string(),
                            }
                        })?;
                        for part in chunk.partitions {
                            decoded_parts.push((part.id, part.messages));
                        }
                    }
                    Ok(decoded_parts)
                }
                .await;
                match load_res {
                    Ok(decoded_parts) => {
                        {
                            let mut part_map = self.partitions.write().await;
                            for (part_id, msgs) in decoded_parts {
                                let entry = part_map.entry(part_id).or_default();
                                entry.extend(msgs);
                                entry.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                            }
                        }
                        self.manager.finish_url_load(url, true).await;
                    }
                    Err(err) => {
                        self.manager.finish_url_load(url, false).await;
                        return Err(err);
                    }
                }
            }

            let batch_rows: Vec<SyncMessage> = {
                let map = self.partitions.read().await;
                map.get(partition)
                    .map(|list| {
                        list.iter()
                            .filter(|m| ts_in_scope(m.timestamp, range.from_ts, cur_to))
                            .take(remaining as usize)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default()
            };
            collected.extend(batch_rows);

            if !scope_match.has_more || collected.len() as u64 >= limit {
                break;
            }
            if let Some(last) = collected.last() {
                if last.timestamp == 0 {
                    break;
                }
                cur_to = last.timestamp.saturating_sub(1);
            } else {
                break;
            }
        }
        Ok(collected)
    }

    /// Closes the message database with `reason`.
    pub async fn close<T: SyncTransport>(&self, transport: &T, reason: DisposeReason) {
        self.manager.release_all(transport, reason).await;
        self.partitions.write().await.clear();
    }
}
