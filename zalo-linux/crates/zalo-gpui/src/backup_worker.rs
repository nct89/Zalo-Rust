//! PC → Phone Message Backup controller, live `BackupTransport` (`630`–`634` +
//! `POST /api/backuppc2mb/upload`), persisted spam/cooldown/interrupt state, 400px modal popup,
//! and sidebar banner.
//!
//! Uses ONLY `zalo-backup` and `libzproto_backup_wasm_bg`. Strictly isolated from Sync 2
//! (`zalo-sync`) and from local zip Export/Import (`pc_export.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::mpsc as futures_mpsc;
use gpui::{
    div, hsla, prelude::*, px, rgb, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zalo_backup::{
    build_and_validate_metadata, build_init_630_payload, build_req_id_only_payload,
    compute_ui_pack_percent, ensure_init_630, ensure_session_before_engine,
    evaluate_confirm_new_backup, fetch_configs_and_status, pack_backup_file,
    trigger_mobile_restore_swallow_errors, upload_backup_file_chunked, BackupBannerState,
    BackupControlEvent, BackupConversationInput, BackupDoneMonitor, BackupErrorCategory,
    BackupInitMetadata, BackupInterruptState, BackupPipelineError, BackupPopupState,
    BackupRemoteConfig, BackupRpcError, BackupSessionState, BackupStatus632, BackupTimingConfig,
    BackupTransport, ConfirmNewBackupDecision, ConfirmSourcePopup, LocalBackupMessage, PackParams,
    SpamRuleTracker, UploadChunkError, UploadChunkRequest, BACKUP_SOCKET_TIMEOUT_MS,
    BACKUP_WASM_FILENAME, CMD_BACKUP_CREATE_SESSION, CMD_BACKUP_GET_CONFIGS,
    CMD_BACKUP_GET_METADATA, CMD_BACKUP_INIT, CMD_BACKUP_SIGNAL_RESTORE,
};
use zalo_client::{ClientError, Device, StoredSession, ZaloApi};
use zalo_storage::{LocalStore, ThreadKind};

use crate::color::{str_vi, token_rgb};
use crate::fonts::FAMILY_SEMIBOLD;
use crate::noise_id::resolve_pc_to_global;
use crate::profiles::Profiles;
use crate::shell::ShellView;
use crate::ws_rpc::{
    ensure_wasm_path, unix_now_ms, WsRpcError, WsRpcHub, EMBEDDED_BACKUP_WASM,
};

const BACKUP_STATE_FILE: &str = "backup-msg-state.json";

/// Persisted per-account backup guard/resume state (maps Electron's `z_backup_sr_`,
/// `z_backup_last_success_at_`, and `z_backup_interrupted_`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedBackupState {
    spam: SpamRuleTracker,
    last_success_at_ms: Option<u64>,
    interrupted: Option<BackupInterruptState>,
}

impl PersistedBackupState {
    fn load(dir: &Path) -> Self {
        let path = dir.join(BACKUP_STATE_FILE);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, dir: &Path) {
        let path = dir.join(BACKUP_STATE_FILE);
        let tmp = dir.join("backup-msg-state.json.tmp");
        if let Ok(text) = serde_json::to_string_pretty(self) {
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

/// Events streamed from background backup tasks back to `ShellView`.
#[derive(Debug, Clone)]
pub enum BackupUiEvent {
    PopupReady(BackupPopupState),
    Banner(BackupBannerState),
    Success {
        file_size: u64,
        completed_at_ms: u64,
    },
    Failed {
        category: BackupErrorCategory,
        can_retry: bool,
        message: String,
        clear_session: bool,
    },
}

/// State held on `ShellView` for PC → Phone Message Backup.
#[derive(Default)]
pub struct BackupUiState {
    pub config: BackupRemoteConfig,
    pub popup: BackupPopupState,
    pub banner: BackupBannerState,
    pub running: bool,
    pub spam: SpamRuleTracker,
    pub last_success_at_ms: Option<u64>,
    pub session: Arc<Mutex<BackupSessionState>>,
    pub last_file_size: u64,
    pub active_existing_size: u64,
    status_fetch_gen: u64,
    services_ready_checked: bool,
}

impl BackupUiState {
    /// Returns whether a PC → Phone backup job is actively running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Returns whether any backup modal popup is open.
    #[must_use]
    pub fn popup_open(&self) -> bool {
        !matches!(self.popup, BackupPopupState::None)
    }

    /// Loads persisted spam/cooldown state from the account directory.
    pub fn load_persisted(&mut self, dir: &Path) {
        let p = PersistedBackupState::load(dir);
        self.spam = p.spam;
        self.last_success_at_ms = p.last_success_at_ms;
        self.session.lock().unwrap().last_success_at_ms = p.last_success_at_ms;
    }

    /// Saves persisted spam/cooldown/interrupt state to the account directory.
    fn save_persisted(&self, dir: &Path, interrupted: Option<BackupInterruptState>) {
        let p = PersistedBackupState {
            spam: self.spam.clone(),
            last_success_at_ms: self.last_success_at_ms,
            interrupted,
        };
        p.save(dir);
    }

    /// Resets UI state on logout.
    pub fn reset(&mut self) {
        self.popup = BackupPopupState::None;
        self.banner = BackupBannerState::Hidden;
        self.running = false;
        self.services_ready_checked = false;
        self.status_fetch_gen = self.status_fetch_gen.wrapping_add(1);
        *self.session.lock().unwrap() = BackupSessionState::default();
    }
}

/// Live `BackupTransport` over `WsRpcHub` (`630`–`634`) and `ZaloApi` (`/api/backuppc2mb/upload`).
struct GpuiBackupTransport {
    api: Arc<ZaloApi>,
}

impl GpuiBackupTransport {
    async fn ws_call(&self, cmd: u16, req_id: &str, body: &Value) -> Result<Value, BackupRpcError> {
        WsRpcHub::global()
            .request(cmd, 0, req_id, body, BACKUP_SOCKET_TIMEOUT_MS)
            .await
            .map_err(map_ws_err)
    }
}

impl BackupTransport for GpuiBackupTransport {
    async fn send_634_get_configs(&self, req_id: &str) -> Result<Value, BackupRpcError> {
        let body = build_req_id_only_payload(req_id);
        self.ws_call(CMD_BACKUP_GET_CONFIGS, req_id, &body).await
    }

    async fn send_632_get_metadata(&self, req_id: &str) -> Result<Value, BackupRpcError> {
        let body = build_req_id_only_payload(req_id);
        self.ws_call(CMD_BACKUP_GET_METADATA, req_id, &body).await
    }

    async fn send_631_create_session(&self, req_id: &str) -> Result<Value, BackupRpcError> {
        let body = build_req_id_only_payload(req_id);
        self.ws_call(CMD_BACKUP_CREATE_SESSION, req_id, &body).await
    }

    async fn send_630_init_session(
        &self,
        req_id: &str,
        meta: &BackupInitMetadata,
        bkid: &str,
        client_salt: &str,
    ) -> Result<Value, BackupRpcError> {
        let body = build_init_630_payload(req_id, meta, bkid, client_salt);
        self.ws_call(CMD_BACKUP_INIT, req_id, &body).await
    }

    async fn send_633_signal_restore(&self, req_id: &str) -> Result<Value, BackupRpcError> {
        let body = build_req_id_only_payload(req_id);
        self.ws_call(CMD_BACKUP_SIGNAL_RESTORE, req_id, &body).await
    }

    async fn upload_chunk<'a>(
        &'a self,
        req: &'a UploadChunkRequest<'a>,
    ) -> Result<(), UploadChunkError> {
        match self
            .api
            .upload_backup_chunk(
                req.bkid,
                req.chunk_id,
                req.total_chunks,
                req.is_last,
                req.file_size,
                req.bytes.to_vec(),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(ClientError::Zalo { code, message }) => {
                Err(UploadChunkError::classify(Some(code), "", &message, false))
            }
            Err(ClientError::Http(err)) => {
                let is_timeout = err.is_timeout();
                let is_net = err.is_connect() || err.is_request();
                let code = if is_timeout {
                    "ERR_CONNECTION_TIMED_OUT"
                } else if is_net {
                    "ERR_NO_NETWORK"
                } else {
                    ""
                };
                Err(UploadChunkError::classify(
                    None,
                    code,
                    &err.to_string(),
                    is_net && !is_timeout,
                ))
            }
            Err(other) => Err(UploadChunkError::classify(
                None,
                "",
                &other.to_string(),
                false,
            )),
        }
    }

    fn is_online(&self) -> bool {
        WsRpcHub::global().is_online()
    }
}

fn map_ws_err(err: WsRpcError) -> BackupRpcError {
    match err {
        WsRpcError::Offline => BackupRpcError::Transport {
            code: "NO_NETWORK".into(),
            message: err.to_string(),
        },
        WsRpcError::Timeout => BackupRpcError::Transport {
            code: "ERR_CONNECTION_TIMED_OUT".into(),
            message: err.to_string(),
        },
        WsRpcError::Disconnected => BackupRpcError::Transport {
            code: "ERR_NO_NETWORK".into(),
            message: err.to_string(),
        },
    }
}

fn load_api_and_device(session_dir: &Path) -> Result<(ZaloApi, String), String> {
    let device = Device::load_or_create(session_dir).map_err(|e| e.to_string())?;
    let session = StoredSession::load(session_dir).map_err(|e| e.to_string())?;
    let name = device.computer_name.clone();
    let api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
    Ok((api, name))
}

impl ShellView {
    /// Applies persisted interrupt state once remote configs arrive (`onServicesReady` in Electron).
    pub(crate) fn on_backup_services_ready(&mut self) {
        if self.backup_ui.services_ready_checked {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        self.backup_ui.services_ready_checked = true;
        let persisted = PersistedBackupState::load(&dir);
        self.backup_ui.spam = persisted.spam.clone();
        self.backup_ui.last_success_at_ms = persisted.last_success_at_ms;
        self.backup_ui.session.lock().unwrap().last_success_at_ms = persisted.last_success_at_ms;

        if !self.backup_ui.config.enabled {
            // Feature flag OFF: clear interrupt record and hide backup UI.
            if persisted.interrupted.is_some() {
                self.backup_ui.save_persisted(&dir, None);
            }
            self.backup_ui.popup = BackupPopupState::None;
            self.backup_ui.banner = BackupBannerState::Hidden;
            return;
        }

        if persisted.interrupted.is_some() {
            // Interrupted on previous run -> show InterruptByDevice banner and clear persisted record.
            self.backup_ui.save_persisted(&dir, None);
            self.backup_ui.banner = BackupBannerState::Fail {
                category: BackupErrorCategory::InterruptByDevice,
                can_retry: true,
                message: "Quá trình sao lưu tin nhắn trước đó bị gián đoạn. Bạn có thể thử lại."
                    .into(),
            };
        }
    }

    /// Opens the PC → Phone Backup modal from Settings (guarded by `backup_msg.enabled` and
    /// mutual exclusion with Sync 2 and local Export/Import).
    pub(crate) fn open_backup_popup(&mut self, cx: &mut Context<Self>) {
        if !self.backup_ui.config.enabled {
            return;
        }
        if self.backup_ui.is_running() || self.sync_ui.is_running() {
            self.set_note(str_vi(
                "STR_BACKUP_MSG_ACTION_BLOCKED_BY_BACKUP_OR_SYNC",
                "Đang có tiến trình đồng bộ hoặc sao lưu chạy. Vui lòng chờ hoàn tất.",
            ));
            cx.notify();
            return;
        }
        if self.export_ui.popup_open() {
            self.set_note(str_vi(
                "STR_BACKUP_MSG_ACTION_BLOCKED_BY_EXPORT_IMPORT",
                "Đang có tiến trình xuất/nhập dữ liệu. Vui lòng chờ hoàn tất.",
            ));
            cx.notify();
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            self.set_note("Chưa đăng nhập tài khoản để sao lưu tin nhắn");
            cx.notify();
            return;
        };

        self.backup_ui.load_persisted(&dir);
        self.backup_ui.status_fetch_gen = self.backup_ui.status_fetch_gen.wrapping_add(1);
        let gen = self.backup_ui.status_fetch_gen;
        let cfg = self.backup_ui.config.clone();
        let session_arc = Arc::clone(&self.backup_ui.session);

        // Electron shows `Loading` ONLY if `634`/`632` takes longer than 1000ms!
        let this_loading = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            gpui::Timer::after(Duration::from_millis(1_000)).await;
            let _ = this_loading.update(cx, |view, cx| {
                if view.backup_ui.status_fetch_gen == gen
                    && matches!(view.backup_ui.popup, BackupPopupState::None)
                    && !view.backup_ui.is_running()
                {
                    view.backup_ui.popup = BackupPopupState::Loading;
                    cx.notify();
                }
            });
        })
        .detach();

        let (ui_tx, mut ui_rx) = futures_mpsc::unbounded();
        crate::ws_rpc::async_rt().spawn(async move {
            let popup = match load_api_and_device(&dir) {
                Ok((api, _)) => {
                    let tr = GpuiBackupTransport { api: Arc::new(api) };
                    let now_ms = unix_now_ms();
                    let req_634 = WsRpcHub::global().next_req_id(CMD_BACKUP_GET_CONFIGS);
                    let req_632 = WsRpcHub::global().next_req_id(CMD_BACKUP_GET_METADATA);
                    let mut st = session_arc.lock().unwrap().clone();
                    let status =
                        fetch_configs_and_status(&tr, &mut st, &cfg, now_ms, &req_634, &req_632)
                            .await;
                    let eff_ttl = cfg.effective_backup_ttl_ms(st.server_configs.as_ref());
                    *session_arc.lock().unwrap() = st;

                    match status {
                        BackupStatus632::None { .. } => BackupPopupState::Intro,
                        BackupStatus632::Existing(info) => {
                            if info.is_locally_expired(now_ms, eff_ttl) {
                                BackupPopupState::Intro
                            } else {
                                BackupPopupState::ExistingBackup(info)
                            }
                        }
                    }
                }
                Err(msg) => BackupPopupState::GenericError { message: msg },
            };
            let _ = ui_tx.unbounded_send(BackupUiEvent::PopupReady(popup));
        });

        let this = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            use futures::StreamExt;
            if let Some(BackupUiEvent::PopupReady(popup)) = ui_rx.next().await {
                let _ = this.update(cx, |view, cx| {
                    if view.backup_ui.status_fetch_gen == gen {
                        if let BackupPopupState::ExistingBackup(ref info) = popup {
                            view.backup_ui.active_existing_size = info.size;
                        }
                        view.backup_ui.popup = popup;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Handles clicking "Tạo bản sao lưu mới" / "Sao lưu ngay" from `Intro` or `ExistingBackup`,
    /// enforcing Electron's exact 5-step `onUserConfirmNewBackup` guard.
    pub(crate) fn confirm_new_backup(
        &mut self,
        source: ConfirmSourcePopup,
        cx: &mut Context<Self>,
    ) {
        let now_ms = unix_now_ms();
        let is_external_busy = self.sync_ui.is_running() || self.export_ui.popup_open();
        let is_online = WsRpcHub::global().is_online();
        let decision = evaluate_confirm_new_backup(
            self.backup_ui.is_running(),
            is_external_busy,
            is_online,
            source,
            self.backup_ui.last_success_at_ms,
            now_ms,
            &self.backup_ui.config,
            &mut self.backup_ui.spam,
        );

        match decision {
            ConfirmNewBackupDecision::IgnoreConcurrent | ConfirmNewBackupDecision::ExternalBusy => {
                // No recordRequest, no popup change.
                if is_external_busy {
                    self.set_note(str_vi(
                        "STR_BACKUP_MSG_ACTION_BLOCKED_BY_BACKUP_OR_SYNC",
                        "Đang có tiến trình khác chạy. Vui lòng chờ hoàn tất.",
                    ));
                    cx.notify();
                }
            }
            ConfirmNewBackupDecision::CooldownToast { remaining_ms } => {
                let mins = (remaining_ms / 60_000).max(1);
                self.set_note(format!(
                    "Bạn vừa tạo bản sao lưu gần đây. Vui lòng thử lại sau khoảng {mins} phút."
                ));
                cx.notify();
            }
            ConfirmNewBackupDecision::SpamToast { remaining_ms } => {
                let secs = (remaining_ms / 1_000).max(1);
                self.set_note(format!(
                    "Bạn đã thao tác quá nhiều lần. Vui lòng thử lại sau {secs} giây."
                ));
                cx.notify();
            }
            ConfirmNewBackupDecision::SpamPopup { unlock_at_ms } => {
                self.backup_ui.popup = BackupPopupState::SpamLocked { unlock_at_ms };
                cx.notify();
            }
            ConfirmNewBackupDecision::OfflineNetworkError => {
                if let Some(ref dir) = self.session_dir {
                    self.backup_ui.save_persisted(dir, None);
                }
                self.backup_ui.popup = BackupPopupState::NetworkError;
                cx.notify();
            }
            ConfirmNewBackupDecision::StartFreshBackup => {
                if let Some(ref dir) = self.session_dir {
                    self.backup_ui.save_persisted(dir, None);
                }
                self.backup_ui.popup = BackupPopupState::None;
                self.start_fresh_backup(cx);
            }
        }
    }

    /// Starts the PC → Phone backup pipeline (`631` → pack with `libzproto_backup_wasm_bg` →
    /// `630` → chunked upload → `hy` verify → Success banner FIRST + QoS-only `backup_done` monitor).
    pub(crate) fn start_fresh_backup(&mut self, cx: &mut Context<Self>) {
        if self.backup_ui.is_running() {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };

        self.backup_ui.running = true;
        self.backup_ui.banner = BackupBannerState::InProgress { percent: 0 };
        self.backup_ui.save_persisted(
            &dir,
            Some(BackupInterruptState {
                bkid: self.backup_ui.session.lock().unwrap().bkid.clone(),
                file_path: None,
                interrupted_at_ms: unix_now_ms(),
                attempts: 0,
                last_uploaded_chunk_id: 0,
            }),
        );
        cx.notify();

        let cfg = self.backup_ui.config.clone();
        let profiles = self.profiles.clone();
        let session_arc = Arc::clone(&self.backup_ui.session);
        let (ui_tx, mut ui_rx) = futures_mpsc::unbounded();

        crate::ws_rpc::async_rt().spawn(async move {
            let outcome = run_backup_pipeline(
                dir.clone(),
                profiles,
                cfg.clone(),
                Arc::clone(&session_arc),
                ui_tx.clone(),
            )
            .await;

            match outcome {
                Ok((bkid, file_size)) => {
                    let completed_at_ms = unix_now_ms();
                    let _ = ui_tx.unbounded_send(BackupUiEvent::Success {
                        file_size,
                        completed_at_ms,
                    });
                    // Arm QoS-only `backup_done` monitor AFTER Success banner is emitted!
                    tokio::spawn(run_qos_backup_done_monitor(bkid, completed_at_ms, cfg));
                }
                Err(err) => {
                    let clear_session = matches!(err, BackupPipelineError::EmptyScopes);
                    let category = err.banner_category();
                    let can_retry = !matches!(
                        err,
                        BackupPipelineError::FileTooLarge { .. }
                            | BackupPipelineError::FeatureDisabled
                    );
                    let _ = ui_tx.unbounded_send(BackupUiEvent::Failed {
                        category,
                        can_retry,
                        message: err.to_string(),
                        clear_session,
                    });
                }
            }
        });

        let this = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            use futures::StreamExt;
            while let Some(ev) = ui_rx.next().await {
                if this
                    .update(cx, |view, cx| {
                        match ev {
                            BackupUiEvent::PopupReady(p) => view.backup_ui.popup = p,
                            BackupUiEvent::Banner(b) => view.backup_ui.banner = b,
                            BackupUiEvent::Success {
                                file_size,
                                completed_at_ms,
                            } => {
                                view.backup_ui.running = false;
                                view.backup_ui.last_file_size = file_size;
                                view.backup_ui.last_success_at_ms = Some(completed_at_ms);
                                view.backup_ui.session.lock().unwrap().last_success_at_ms =
                                    Some(completed_at_ms);
                                if let Some(ref dir) = view.session_dir {
                                    view.backup_ui.save_persisted(dir, None);
                                }
                                // Success banner is ALWAYS shown first; NEVER calls 633 here!
                                view.backup_ui.banner = BackupBannerState::Success { file_size };
                            }
                            BackupUiEvent::Failed {
                                category,
                                can_retry,
                                message,
                                clear_session,
                            } => {
                                view.backup_ui.running = false;
                                if clear_session {
                                    view.backup_ui.session.lock().unwrap().clear_session_keys();
                                }
                                if let Some(ref dir) = view.session_dir {
                                    view.backup_ui.save_persisted(dir, None);
                                }
                                view.backup_ui.banner = BackupBannerState::Fail {
                                    category,
                                    can_retry,
                                    message,
                                };
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// `onUserConfirmRestoreOnMobile`: hides `ExistingBackup` popup, shows `AfterSuccess` banner
    /// FIRST, then sends `633` swallowing all errors (`My()` in Electron).
    pub(crate) fn confirm_restore_on_mobile(&mut self, cx: &mut Context<Self>) {
        let size = self.backup_ui.active_existing_size;
        self.backup_ui.popup = BackupPopupState::None;
        self.backup_ui.banner = BackupBannerState::AfterSuccess { file_size: size };
        cx.notify();
        self.fire_633_swallow_errors();
    }

    /// `onUserStartMobileRestore`: transitions `Success` banner → `AfterSuccess` banner FIRST,
    /// then sends `633` swallowing all errors (`My()` in Electron).
    pub(crate) fn start_mobile_restore(&mut self, cx: &mut Context<Self>) {
        let size = self.backup_ui.last_file_size;
        self.backup_ui.banner = BackupBannerState::AfterSuccess { file_size: size };
        cx.notify();
        self.fire_633_swallow_errors();
    }

    fn fire_633_swallow_errors(&self) {
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        crate::ws_rpc::async_rt().spawn(async move {
            if let Ok((api, _)) = load_api_and_device(&dir) {
                let tr = GpuiBackupTransport { api: Arc::new(api) };
                let req_id = WsRpcHub::global().next_req_id(CMD_BACKUP_SIGNAL_RESTORE);
                let _ok = trigger_mobile_restore_swallow_errors(&tr, &req_id).await;
            }
        });
    }

    /// Sidebar Backup banner rendered below `list_tabs` / `sync_banner_el`.
    pub(crate) fn backup_banner_el(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let dark = crate::color::is_dark();
        let bg = if dark { 0x1e_29_3b } else { 0xe8_f2_ff };
        let ok_bg = if dark { 0x17_33_26 } else { 0xe8_f7_ee };
        let err_bg = if dark { 0x3b_1f_22 } else { 0xfd_ed_ed };
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let border = if dark { 0x33_41_55 } else { 0xc7_dd_ff };

        match &self.backup_ui.banner {
            BackupBannerState::Hidden => None,
            BackupBannerState::WaitForNetwork => Some(
                div()
                    .id("backup-banner-wait-net")
                    .mx(px(12.))
                    .my(px(6.))
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(bg))
                    .border_1()
                    .border_color(rgb(border))
                    .text_size(px(12.))
                    .text_color(rgb(ink))
                    .child("Đang chờ kết nối mạng để tiếp tục sao lưu tin nhắn…")
                    .into_any_element(),
            ),
            BackupBannerState::InProgress { percent } => {
                let pct = (*percent).min(100);
                Some(
                    div()
                        .id("backup-banner-progress")
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .mx(px(12.))
                        .my(px(6.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(rgb(bg))
                        .border_1()
                        .border_color(rgb(border))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(ink))
                                .child(format!("Đang sao lưu tin nhắn ({pct}%)")),
                        )
                        .child(
                            div()
                                .w_full()
                                .h(px(4.))
                                .rounded(px(2.))
                                .bg(rgb(border))
                                .child(
                                    div()
                                        .h_full()
                                        .w(gpui::relative(pct as f32 / 100.0))
                                        .rounded(px(2.))
                                        .bg(rgb(blue)),
                                ),
                        )
                        .into_any_element(),
                )
            }
            BackupBannerState::Success { file_size } => Some(
                div()
                    .id("backup-banner-success")
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .mx(px(12.))
                    .my(px(6.))
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(ok_bg))
                    .border_1()
                    .border_color(rgb(border))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child("Tạo bản sao lưu tin nhắn thành công"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(muted))
                            .child(format!(
                                "Dung lượng: {}. Nhấn tiếp tục để khôi phục trên điện thoại.",
                                format_bytes(*file_size)
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("backup-continue-phone-btn")
                                    .px(px(10.))
                                    .py(px(4.))
                                    .rounded(px(4.))
                                    .bg(rgb(blue))
                                    .text_size(px(12.))
                                    .text_color(rgb(0xff_ff_ff))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.start_mobile_restore(cx);
                                    }))
                                    .child("Tiếp tục trên điện thoại"),
                            )
                            .child(
                                div()
                                    .id("backup-success-close")
                                    .px(px(10.))
                                    .py(px(4.))
                                    .text_size(px(12.))
                                    .text_color(rgb(muted))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.backup_ui.banner = BackupBannerState::Hidden;
                                        cx.notify();
                                    }))
                                    .child("Đóng"),
                            ),
                    )
                    .into_any_element(),
            ),
            BackupBannerState::AfterSuccess { .. } => Some(
                div()
                    .id("backup-banner-after-success")
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .mx(px(12.))
                    .my(px(6.))
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(ok_bg))
                    .border_1()
                    .border_color(rgb(border))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(ink))
                            .child("Mở Zalo trên điện thoại để hoàn tất khôi phục tin nhắn."),
                    )
                    .child(
                        div()
                            .id("backup-after-close")
                            .text_size(px(12.))
                            .text_color(rgb(blue))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.backup_ui.banner = BackupBannerState::Hidden;
                                cx.notify();
                            }))
                            .child("Đóng"),
                    )
                    .into_any_element(),
            ),
            BackupBannerState::Fail {
                can_retry, message, ..
            } => {
                let msg_text = message.clone();
                let retryable = *can_retry;
                Some(
                    div()
                        .id("backup-banner-fail")
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .mx(px(12.))
                        .my(px(6.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(rgb(err_bg))
                        .border_1()
                        .border_color(rgb(border))
                        .child(div().text_size(px(12.)).text_color(rgb(ink)).child(msg_text))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(8.))
                                .when(retryable, |el| {
                                    el.child(
                                        div()
                                            .id("backup-retry-btn")
                                            .px(px(10.))
                                            .py(px(4.))
                                            .rounded(px(4.))
                                            .bg(rgb(blue))
                                            .text_size(px(12.))
                                            .text_color(rgb(0xff_ff_ff))
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.start_fresh_backup(cx);
                                            }))
                                            .child("Thử lại"),
                                    )
                                })
                                .child(
                                    div()
                                        .id("backup-fail-close")
                                        .px(px(10.))
                                        .py(px(4.))
                                        .text_size(px(12.))
                                        .text_color(rgb(muted))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.backup_ui.banner = BackupBannerState::Hidden;
                                            cx.notify();
                                        }))
                                        .child("Đóng"),
                                ),
                        )
                        .into_any_element(),
                )
            }
            BackupBannerState::Interrupted {
                attempt,
                max_attempts,
            } => Some(
                div()
                    .id("backup-banner-interrupted")
                    .mx(px(12.))
                    .my(px(6.))
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(bg))
                    .border_1()
                    .border_color(rgb(border))
                    .text_size(px(12.))
                    .text_color(rgb(ink))
                    .child(format!(
                        "Sao lưu bị gián đoạn, đang tự động thử lại ({attempt}/{max_attempts})…"
                    ))
                    .into_any_element(),
            ),
        }
    }

    /// 400px PC → Phone Backup modal popup overlay (`compact3197.js` enum `i`).
    pub(crate) fn backup_popup_overlay(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let dark = crate::color::is_dark();
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let border = if dark { 0x3a_3e_46 } else { 0xe1_e5_ea };
        let sec_bg = if dark { 0x2a_2c_31 } else { 0xf1_f2_f4 };

        let body: gpui::AnyElement = match &self.backup_ui.popup {
            BackupPopupState::None => div().into_any_element(),
            BackupPopupState::Loading => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .py(px(20.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(ink))
                        .child("Đang kiểm tra bản sao lưu…"),
                )
                .into_any_element(),
            BackupPopupState::Intro => div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(ink))
                        .child(str_vi(
                            "STR_BACKUP_METHOD_SETTING_TITLE",
                            "Sao lưu tin nhắn sang điện thoại",
                        )),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .line_height(px(19.))
                        .text_color(rgb(muted))
                        .child("Tạo bản sao lưu tin nhắn được mã hóa từ máy tính này để khôi phục trên điện thoại của bạn."),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.))
                        .mt(px(8.))
                        .child(
                            div()
                                .id("backup-intro-later")
                                .px(px(14.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(sec_bg))
                                .text_size(px(13.))
                                .text_color(rgb(ink))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.backup_ui.popup = BackupPopupState::None;
                                    cx.notify();
                                }))
                                .child("Để sau"),
                        )
                        .child(
                            div()
                                .id("backup-intro-create")
                                .px(px(14.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(blue))
                                .text_size(px(13.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(0xff_ff_ff))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_new_backup(ConfirmSourcePopup::Intro, cx);
                                }))
                                .child("Tạo bản sao lưu"),
                        ),
                )
                .into_any_element(),
            BackupPopupState::ExistingBackup(info) => {
                let dev = if info.device_name.is_empty() {
                    "Máy tính".to_owned()
                } else {
                    info.device_name.clone()
                };
                let size_str = format_bytes(info.size);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child("Đã có bản sao lưu tin nhắn"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .p(px(10.))
                            .rounded(px(6.))
                            .bg(rgb(sec_bg))
                            .text_size(px(13.))
                            .text_color(rgb(ink))
                            .child(format!("Thiết bị: {dev}"))
                            .child(format!("Dung lượng: {size_str}")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.))
                            .mt(px(8.))
                            .child(
                                div()
                                    .id("backup-exist-new")
                                    .px(px(12.))
                                    .py(px(8.))
                                    .rounded(px(6.))
                                    .bg(rgb(sec_bg))
                                    .text_size(px(13.))
                                    .text_color(rgb(ink))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_new_backup(
                                            ConfirmSourcePopup::ExistingBackup,
                                            cx,
                                        );
                                    }))
                                    .child("Tạo bản mới"),
                            )
                            .child(
                                div()
                                    .id("backup-exist-use")
                                    .px(px(12.))
                                    .py(px(8.))
                                    .rounded(px(6.))
                                    .bg(rgb(blue))
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(0xff_ff_ff))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_restore_on_mobile(cx);
                                    }))
                                    .child("Dùng bản này"),
                            ),
                    )
                    .into_any_element()
            }
            BackupPopupState::NetworkError => div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(ink))
                        .child("Không có kết nối mạng"),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .child("Vui lòng kiểm tra kết nối Internet và thử lại."),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .child(
                            div()
                                .id("backup-net-close")
                                .px(px(14.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(blue))
                                .text_size(px(13.))
                                .text_color(rgb(0xff_ff_ff))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.backup_ui.popup = BackupPopupState::None;
                                    cx.notify();
                                }))
                                .child("Đóng"),
                        ),
                )
                .into_any_element(),
            BackupPopupState::GenericError { message } => {
                let msg_text = message.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child("Không thể sao lưu tin nhắn"),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child(msg_text),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .child(
                                div()
                                    .id("backup-gen-close")
                                    .px(px(14.))
                                    .py(px(8.))
                                    .rounded(px(6.))
                                    .bg(rgb(blue))
                                    .text_size(px(13.))
                                    .text_color(rgb(0xff_ff_ff))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.backup_ui.popup = BackupPopupState::None;
                                        cx.notify();
                                    }))
                                    .child("Đóng"),
                            ),
                    )
                    .into_any_element()
            }
            BackupPopupState::SpamLocked { unlock_at_ms } => {
                let rem_secs = unlock_at_ms
                    .saturating_sub(unix_now_ms())
                    .div_ceil(1_000)
                    .max(1);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child("Tạm giới hạn tạo bản sao lưu"),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child(format!(
                                "Bạn đã yêu cầu tạo bản sao lưu quá nhiều lần. Vui lòng thử lại sau khoảng {rem_secs} giây."
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .child(
                                div()
                                    .id("backup-spam-close")
                                    .px(px(14.))
                                    .py(px(8.))
                                    .rounded(px(6.))
                                    .bg(rgb(blue))
                                    .text_size(px(13.))
                                    .text_color(rgb(0xff_ff_ff))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.backup_ui.popup = BackupPopupState::None;
                                        cx.notify();
                                    }))
                                    .child("Đã hiểu"),
                            ),
                    )
                    .into_any_element()
            }
        };

        div()
            .id("backup-popup-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.45))
            .on_click(cx.listener(|this, _, _, cx| {
                this.backup_ui.popup = BackupPopupState::None;
                cx.notify();
            }))
            .child(
                div()
                    .id("backup-popup-card")
                    .w(px(400.))
                    .p(px(20.))
                    .rounded(px(8.))
                    .bg(rgb(paper))
                    .border_1()
                    .border_color(rgb(border))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(body),
            )
            .into_any_element()
    }
}

async fn run_backup_pipeline(
    session_dir: PathBuf,
    profiles: Profiles,
    cfg: BackupRemoteConfig,
    session_arc: Arc<Mutex<BackupSessionState>>,
    ui_tx: futures_mpsc::UnboundedSender<BackupUiEvent>,
) -> Result<(String, u64), BackupPipelineError> {
    if !cfg.enabled {
        return Err(BackupPipelineError::FeatureDisabled);
    }
    let (api, device_name) =
        load_api_and_device(&session_dir).map_err(BackupPipelineError::Io)?;
    let api = Arc::new(api);
    let transport = GpuiBackupTransport {
        api: Arc::clone(&api),
    };
    let timing = BackupTimingConfig::default();

    // Step 1: Ensure 631 session + local 32-byte clientSalt before engine (oy reuse rule).
    let mut state = session_arc.lock().unwrap().clone();
    let (bkid, server_nonce, client_salt) = ensure_session_before_engine(
        &transport,
        &mut state,
        &cfg,
        unix_now_ms(),
        timing,
        |_| WsRpcHub::global().next_req_id(CMD_BACKUP_CREATE_SESSION),
    )
    .await?;
    *session_arc.lock().unwrap() = state.clone();

    // Step 2: Load conversations from LocalStore and resolve PC -> Global Noise IDs (GET_BACKUP_NOISED_IDS).
    let store_path = session_dir.join("store.db");
    let (_, self_uid) = WsRpcHub::global().session_ids();
    let conversations: Vec<BackupConversationInput> = {
        let store = LocalStore::open(&store_path)
            .map_err(|e| BackupPipelineError::Io(e.to_string()))?;
        store
            .threads()
            .map_err(|e| BackupPipelineError::Io(e.to_string()))?
            .into_iter()
            .filter(|t| !t.thread_id.is_empty() && !t.thread_id.starts_with("preview:"))
            .map(|t| BackupConversationInput {
                pc_conv_id: t.thread_id,
                is_group: t.kind == ThreadKind::Group,
            })
            .collect()
    };

    let mut ids_to_resolve: Vec<String> = conversations
        .iter()
        .map(|c| c.pc_conv_id.clone())
        .collect();
    if !self_uid.is_empty() {
        ids_to_resolve.push(self_uid.clone());
    }
    let noise_map = resolve_pc_to_global(&api, &profiles, &ids_to_resolve).await;
    let my_global_noised_id = noise_map
        .get(&self_uid)
        .cloned()
        .or_else(|| {
            profiles
                .map
                .get(crate::profiles::ME)
                .map(|p| p.global_id.clone())
                .filter(|s| !s.is_empty())
        });

    // Step 3: Pack messages into `<session_dir>/backup_pc2mb/0` using ONLY libzproto_backup_wasm_bg!
    let wasm_path = if cfg.encryption_enabled {
        Some(
            ensure_wasm_path(BACKUP_WASM_FILENAME, EMBEDDED_BACKUP_WASM).ok_or_else(|| {
                BackupPipelineError::Io(format!(
                    "Không tìm thấy tệp mã hóa sao lưu ({BACKUP_WASM_FILENAME})"
                ))
            })?,
        )
    } else {
        None
    };

    let backup_dir = session_dir.join("backup_pc2mb");
    let weight = cfg.backup_progress_weight;
    let enc_enabled = cfg.encryption_enabled;
    let ui_pack = ui_tx.clone();
    let bkid_pack = bkid.clone();
    let nonce_pack = server_nonce.clone();
    let salt_pack = client_salt.clone();

    let pack_res = tokio::task::spawn_blocking(move || {
        let store = LocalStore::open(&store_path)
            .map_err(|e| BackupPipelineError::Io(e.to_string()))?;
        let attachments_map: HashMap<i64, String> = store
            .attachments()
            .unwrap_or_default()
            .into_iter()
            .collect();

        pack_backup_file(
            &PackParams {
                backup_dir: &backup_dir,
                bkid: &bkid_pack,
                server_nonce_b64: &nonce_pack,
                client_salt_b64: &salt_pack,
                encryption_enabled: enc_enabled,
                my_pc_uid: &self_uid,
                my_global_noised_id: my_global_noised_id.as_deref(),
                conversations: &conversations,
                noise_map: &noise_map,
            },
            wasm_path.as_deref(),
            |scope| {
                let rows = store.messages(&scope.pc_conv_id).unwrap_or_default();
                rows.into_iter()
                    .map(|r| {
                        let msg_type_str = attachments_map
                            .get(&r.message_id)
                            .and_then(|s| serde_json::from_str::<Value>(s).ok())
                            .and_then(|v| {
                                v.get("msgType")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                            })
                            .unwrap_or_else(|| "webchat".to_owned());
                        LocalBackupMessage {
                            sender_pc_id: r.sender_id.unwrap_or_else(|| "0".to_owned()),
                            client_id: r.cli_msg_id.unwrap_or(0).max(0) as u64,
                            pc_status: 1,
                            pc_msg_type: None,
                            msg_type_str,
                            timestamp_ms: r.sent_at.max(0) as u64,
                            global_id: r
                                .global_msg_id
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "0".to_owned()),
                            content: r.body,
                            ..Default::default()
                        }
                    })
                    .collect()
            },
            |worker_pct| {
                let ui_pct = compute_ui_pack_percent(worker_pct, weight);
                let _ = ui_pack.unbounded_send(BackupUiEvent::Banner(
                    BackupBannerState::InProgress { percent: ui_pct },
                ));
            },
        )
        .map_err(|e| BackupPipelineError::Io(e.to_string()))
    })
    .await
    .map_err(|e| BackupPipelineError::Io(e.to_string()))??;

    // Step 4: Validate metadata before 630 (scope_count == 0 -> UserSystem + reset!).
    let meta = build_and_validate_metadata(&pack_res, &device_name, &cfg, &mut state)?;
    *session_arc.lock().unwrap() = state.clone();

    // Step 5: Socket 630 (`oy = 1_740_000` reuse window & error 212 reset).
    let init_res = ensure_init_630(&transport, &mut state, &meta, unix_now_ms(), timing, |_| {
        WsRpcHub::global().next_req_id(CMD_BACKUP_INIT)
    })
    .await;
    *session_arc.lock().unwrap() = state.clone();
    init_res?;

    // Step 6: Chunked upload (`/api/backuppc2mb/upload`) + post-upload `hy` verification.
    let ui_up = ui_tx.clone();
    let up_res = upload_backup_file_chunked(&transport, &mut state, &cfg, timing, |ui_pct| {
        let _ = ui_up.unbounded_send(BackupUiEvent::Banner(BackupBannerState::InProgress {
            percent: ui_pct,
        }));
    })
    .await;
    *session_arc.lock().unwrap() = state;
    let outcome = up_res?;

    Ok((outcome.bkid, outcome.file_size))
}

/// QoS-only `armBackupDoneMonitor` (180s). NEVER controls whether the Success banner is displayed!
async fn run_qos_backup_done_monitor(bkid: String, armed_at_ms: u64, cfg: BackupRemoteConfig) {
    let mut monitor = BackupDoneMonitor::arm(bkid.clone(), armed_at_ms, &cfg);
    let mut rx = WsRpcHub::global().subscribe_backup_controls();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(cfg.backup_done_timeout_seconds);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                monitor.check_timeout(unix_now_ms());
                eprintln!("zalo-gpui backup monitor: bkid={bkid} status={:?}", monitor.status);
                break;
            }
            ev = rx.recv() => {
                if let Ok(BackupControlEvent::BackupDone { bkid: ev_bkid, err }) = ev {
                    if monitor.on_backup_done(&ev_bkid, err) {
                        eprintln!("zalo-gpui backup monitor: bkid={bkid} status={:?}", monitor.status);
                        break;
                    }
                }
            }
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
