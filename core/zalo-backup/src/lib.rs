//! Zalo PC Message Backup crate (PC → Phone: socket `630`–`634`, `/api/backuppc2mb/upload`,
//! `libzproto_backup_wasm_bg` encryption, `proto.Sync2.Message.SyncChunk`, guards and pipeline).
//!
//! Strictly decoupled from Sync 2 (`zalo-sync`) and from local zip Export/Import (`pc_export.rs`).

#![forbid(unsafe_code)]
#![allow(missing_docs)]

pub mod config;
pub mod guard;
pub mod packer;
pub mod pipeline;
pub mod protocol;
pub mod wasm;

#[cfg(test)]
mod tests;

pub use config::{
    seconds_to_ms_with_sentinel, BackupRemoteConfig, ServerBackupConfigs,
    DEFAULT_AUTO_RESUME_BACKOFF_MS, UPLOAD_REUSE_WINDOW_MS,
};
pub use guard::{
    check_cooldown, evaluate_confirm_new_backup, BackupBannerState, BackupErrorCategory,
    BackupPopupState, ConfirmNewBackupDecision, ConfirmSourcePopup, SpamRuleTracker,
};
pub use packer::{
    append_framed_block, backup_block_file_path, build_message_scopes,
    compute_file_sha256_and_size, compute_ui_pack_percent, compute_ui_upload_percent,
    compute_worker_progress_percent, drop_unnoisable_scopes, encode_message_sync_chunk,
    format_wire_partition, map_pc_msg_status, map_pc_numeric_msg_type, map_string_msg_type,
    pack_backup_file, read_framed_blocks, transform_message, BackupAttach,
    BackupConversationInput, BackupMention, BackupMessageScope, BackupProperties, BackupQuote,
    BackupReference, FilteredScopes, LocalBackupMessage, PackParams, PackResult, PackerError,
    WireBackupMessage, JS_MAX_SAFE_INTEGER, MAX_SCOPE_PAGES, MESSAGE_SCOPE_LIMIT,
    PLATFORM_TYPE_PC, SCOPE_PAGE_SIZE,
};
pub use pipeline::{
    build_and_validate_metadata, call_631_with_retry, ensure_init_630,
    ensure_session_before_engine, fetch_configs_and_status, get_resume_step_for,
    next_auto_resume_delay_ms, trigger_mobile_restore_swallow_errors, upload_backup_file_chunked,
    verify_post_upload_integrity, BackupDoneMonitor, BackupDoneMonitorStatus, BackupInterruptState,
    BackupPipelineError, BackupSessionState, BackupTimingConfig, BackupTransport,
    BackupUploadOutcome, ChunkRetryKind, ResumeStep, UploadChunkError, UploadChunkRequest,
};
pub use protocol::{
    build_init_630_payload, build_req_id_only_payload, generate_client_salt,
    is_retryable_init_code_or_msg, parse_630_response, parse_631_response, parse_632_status,
    parse_633_response, parse_634_configs, parse_backup_controls, BackupControlEvent,
    BackupInit630, BackupInitMetadata, BackupRpcError, BackupSession631, BackupStatus632,
    ExistingBackupInfo, BACKUP_INIT_MAX_ATTEMPTS, BACKUP_INIT_RETRY_DELAY_MS,
    BACKUP_SOCKET_TIMEOUT_MS, CMD_BACKUP_CREATE_SESSION, CMD_BACKUP_GET_CONFIGS,
    CMD_BACKUP_GET_METADATA, CMD_BACKUP_INIT, CMD_BACKUP_SIGNAL_RESTORE, ERR_630_INVALID_SESSION,
    ERR_632_NO_BACKUP,
};
pub use wasm::{
    derive_backup_wasm_key, BackupCryptoSession, BackupWasm, BackupWasmError, BACKUP_WASM_FILENAME,
};
