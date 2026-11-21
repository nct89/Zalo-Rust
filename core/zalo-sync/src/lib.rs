//! Zalo PC Sync 2 (phone → PC message synchronization).
//!
//! Ported strictly from Electron's `compact0909.js`, `compact2101.js`, `compact2186.js`,
//! `compact3613.js`, `compact3272.js`, `compact3857.js`, `main-startup`, and `shared-worker`.
//!
//! This crate handles ONLY phone → PC message sync (socket `590`/`591`/`592` and
//! `libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm`).
//! PC → phone backup (`630`–`634`) lives in the separate `zalo-backup` crate.

#![forbid(unsafe_code)]

pub mod config;
pub mod engine;
pub mod proto;
pub mod protocol;
pub mod session;
pub mod wasm;

#[cfg(test)]
mod tests;

pub use config::{
    SyncBranchConfig, SyncRemoteConfig, SyncSource, TimeRange, TimeoutConfigs,
    JS_MAX_SAFE_INTEGER, SYNC_UNLIMITED_LIMIT,
};
pub use engine::{
    classify_mobile_status, compute_completion_percent, exceeded_max_percent_batch_fail,
    sync_conversations_macro, sync_other_messages_macro, ConversationSyncOutput,
    MessagesSyncOutput, MissingMessageRange, MobileStatusDisposition, SyncFailReason, SyncProgress,
};
pub use proto::{
    read_framed_blocks, sync_message_to_server_json, write_framed_blocks, zstd_compress,
    zstd_decompress, ConvType, ConversationSyncChunk, MessageSyncChunk, PlatformType,
    SyncConversation, SyncMessage,
};
pub use protocol::{
    build_591_body, build_592_body, format_partition, generate_sync_id, parse_sync_controls,
    retry_backoff_ms, should_retry_cmd, BatchScope, BatchType, DeviceType, DisposeReason,
    MobileStatus, MobileStatusPayload, RpcErrorKind, SyncControlEvent, SyncDatabaseKind,
    SyncPriority, SyncRequest590, SyncType, TransferAfterLoginEvent, UploadBatchPayload, WireQuery,
    CMD_SYNC_ACK_DELETE, CMD_SYNC_REQUEST, CMD_SYNC_WAKEUP, TIMEOUT_SYNC_CONTROL_MS,
    TIMEOUT_SYNC_REQUEST_MS, TRANSFER_AFTER_LOGIN_MAX_AGE_MS,
};
pub use session::{
    group_prepare_queries, ConversationRemoteDb, MessageRemoteDb, SessionManager, SessionOptions,
    SessionQuerySpec, SyncSession, SyncSessionError, SyncTransport, UssIdxCounter,
};
pub use wasm::{KeyPair, SyncWasm, SyncWasmError, SYNC_WASM_FILENAME};
