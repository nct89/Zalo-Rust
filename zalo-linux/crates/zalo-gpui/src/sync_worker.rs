//! Phone → PC Message Sync (Sync 2) controller, background worker, live transport adapter,
//! and sidebar progress banner.
//!
//! Uses ONLY `zalo-sync` (`590`/`591`/`592` + `libzproto_wasm_bg`). Never touches `zalo-backup`
//! or local zip Export/Import (`pc_export.rs`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use futures::channel::mpsc as futures_mpsc;
use gpui::{
    div, prelude::*, px, rgb, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};
use serde_json::Value;
use tokio::sync::mpsc as tokio_mpsc;
use zalo_client::{Device, StoredSession, ZaloApi};
use zalo_storage::{LocalStore, MessageRecord, ThreadKind, ThreadRecord};
use zalo_sync::{
    build_591_body, build_592_body, classify_mobile_status, sync_conversations_macro,
    sync_message_to_server_json, sync_other_messages_macro, ConvType, ConversationRemoteDb,
    DeviceType, DisposeReason, KeyPair, MessageRemoteDb, MissingMessageRange,
    MobileStatusDisposition, SessionManager, SessionOptions, SyncControlEvent, SyncConversation,
    SyncFailReason, SyncMessage, SyncRemoteConfig, SyncRequest590, SyncSessionError,
    SyncSource, SyncTransport, SyncWasm, TimeRange, TransferAfterLoginEvent, UssIdxCounter,
    CMD_SYNC_ACK_DELETE, CMD_SYNC_REQUEST, CMD_SYNC_WAKEUP, JS_MAX_SAFE_INTEGER,
    SYNC_WASM_FILENAME, TIMEOUT_SYNC_CONTROL_MS, TIMEOUT_SYNC_REQUEST_MS,
};

use crate::color::{str_vi, token_rgb};
use crate::fonts::FAMILY_SEMIBOLD;
use crate::noise_id::resolve_global_to_pc;
use crate::profiles::Profiles;
use crate::shell::ShellView;
use crate::ws_rpc::{async_rt, locate_wasm, unix_now_ms, WsRpcError, WsRpcHub};

/// Session-scoped in-memory Identity Key (`Bw1J` in `compact1016.js`): generated once per login
/// session and cleared on logout.
fn session_ik_cell() -> &'static Mutex<Option<KeyPair>> {
    static CELL: OnceLock<Mutex<Option<KeyPair>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// Clears the in-memory Sync 2 Identity Key on logout or account switch.
pub fn reset_sync_identity_key() {
    *session_ik_cell().lock().unwrap() = None;
}

fn get_or_generate_ik(wasm: &Mutex<SyncWasm>) -> Result<KeyPair, String> {
    let mut guard = session_ik_cell().lock().unwrap();
    if let Some(ref kp) = *guard {
        return Ok(kp.clone());
    }
    let kp = wasm
        .lock()
        .unwrap()
        .gen_keypair()
        .map_err(|e| format!("gen_keypair failed: {e}"))?;
    *guard = Some(kp.clone());
    Ok(kp)
}

/// UI banner state for Phone → PC Sync 2.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SyncBannerState {
    #[default]
    Hidden,
    /// Waiting for user confirmation on mobile phone (no 180s timer; wakeup via `592`).
    WaitingConfirm,
    /// Syncing conversations / messages with truthful percentage (`isCompleted` ratio only).
    Syncing { percent: Option<u8> },
    /// Sync completed cleanly.
    Completed { conv_count: usize, msg_count: usize },
    /// Sync failed or cancelled.
    Error { message: String, can_retry: bool },
}

/// Commands sent from the UI to the running Sync 2 worker task.
#[derive(Debug)]
enum SyncWorkerCmd {
    Wakeup,
    Cancel,
}

/// Events emitted by the Sync 2 worker back to `ShellView`.
#[derive(Debug, Clone)]
pub enum SyncUiEvent {
    WaitingConfirm,
    Progress { percent: Option<u8> },
    DataUpdated,
    Completed { conv_count: usize, msg_count: usize },
    Cancelled,
    Error { message: String, can_retry: bool },
}

/// State held on `ShellView` for Phone → PC Sync 2.
#[derive(Default)]
pub struct SyncUiState {
    pub config: SyncRemoteConfig,
    pub banner: SyncBannerState,
    pub running: bool,
    cmd_tx: Option<tokio_mpsc::UnboundedSender<SyncWorkerCmd>>,
}

impl SyncUiState {
    /// Returns whether a Sync 2 run is actively in progress.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Cancels an active Sync 2 session (`591` with `DisposeReason::UserCancel`).
    pub fn cancel(&mut self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(SyncWorkerCmd::Cancel);
        }
    }

    /// Sends a `592` wakeup request on the active Sync 2 session.
    pub fn wakeup(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(SyncWorkerCmd::Wakeup);
        }
    }

    /// Resets UI state on logout.
    pub fn reset(&mut self) {
        self.cancel();
        self.running = false;
        self.cmd_tx = None;
        self.banner = SyncBannerState::Hidden;
        reset_sync_identity_key();
    }
}

/// Live `SyncTransport` over `WsRpcHub` (`590`/`591`/`592`) and `ZaloApi` (`download_sync_batch`).
struct GpuiSyncTransport {
    api: Arc<ZaloApi>,
}

impl SyncTransport for GpuiSyncTransport {
    async fn send_request_590(&self, req: &SyncRequest590) -> Result<(), SyncSessionError> {
        let hub = WsRpcHub::global();
        let req_id = hub.next_req_id(CMD_SYNC_REQUEST);
        let body = req.to_frame_body(&req_id);
        let resp = hub
            .request(CMD_SYNC_REQUEST, 0, &req_id, &body, TIMEOUT_SYNC_REQUEST_MS)
            .await
            .map_err(map_ws_err)?;
        check_sync_rpc_ok(&resp)
    }

    async fn send_delete_591(
        &self,
        sync_id: &str,
        to_device: DeviceType,
        reason: DisposeReason,
    ) -> Result<(), SyncSessionError> {
        let hub = WsRpcHub::global();
        let req_id = hub.next_req_id(CMD_SYNC_ACK_DELETE);
        let body = build_591_body(&req_id, sync_id, to_device, reason);
        let _ = hub
            .request(CMD_SYNC_ACK_DELETE, 0, &req_id, &body, TIMEOUT_SYNC_CONTROL_MS)
            .await;
        Ok(())
    }

    async fn send_wakeup_592(
        &self,
        sync_id: &str,
        to_device: DeviceType,
    ) -> Result<i64, SyncSessionError> {
        let hub = WsRpcHub::global();
        let req_id = hub.next_req_id(CMD_SYNC_WAKEUP);
        let body = build_592_body(&req_id, sync_id, to_device);
        let resp = hub
            .request(CMD_SYNC_WAKEUP, 0, &req_id, &body, TIMEOUT_SYNC_REQUEST_MS)
            .await
            .map_err(map_ws_err)?;
        let code = resp
            .get("error_code")
            .and_then(Value::as_i64)
            .or_else(|| resp.pointer("/data/code").and_then(Value::as_i64))
            .unwrap_or(0);
        Ok(code)
    }

    async fn download_batch(&self, url: &str) -> Result<Vec<u8>, SyncSessionError> {
        self.api
            .download_sync_batch(url)
            .await
            .map_err(|e| SyncSessionError::BatchDecode {
                step: "Download",
                message: e.to_string(),
            })
    }
}

fn map_ws_err(err: WsRpcError) -> SyncSessionError {
    match err {
        WsRpcError::Timeout => SyncSessionError::ConnectTimeout,
        WsRpcError::Offline | WsRpcError::Disconnected => SyncSessionError::Remote {
            code: -2,
            message: err.to_string(),
        },
    }
}

fn check_sync_rpc_ok(resp: &Value) -> Result<(), SyncSessionError> {
    let outer = resp.get("error_code").and_then(Value::as_i64).unwrap_or(0);
    if outer != 0 {
        let msg = resp
            .get("error_message")
            .and_then(Value::as_str)
            .unwrap_or("sync rpc error")
            .to_owned();
        return Err(SyncSessionError::Remote {
            code: outer,
            message: msg,
        });
    }
    let inner = resp
        .pointer("/data/err")
        .or_else(|| resp.pointer("/data/code"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if inner != 0 {
        return Err(SyncSessionError::Remote {
            code: inner,
            message: format!("sync rpc inner error {inner}"),
        });
    }
    Ok(())
}

/// Spawns one Phone → PC Sync 2 run on the Tokio runtime and streams [`SyncUiEvent`]s back.
fn spawn_sync_job(
    session_dir: PathBuf,
    profiles: Profiles,
    cfg: SyncRemoteConfig,
    source: SyncSource,
    temp_key: String,
    ui_tx: futures_mpsc::UnboundedSender<SyncUiEvent>,
) -> tokio_mpsc::UnboundedSender<SyncWorkerCmd> {
    let (cmd_tx, mut cmd_rx) = tokio_mpsc::unbounded_channel::<SyncWorkerCmd>();

    async_rt().spawn(async move {
        let res = run_sync_job(
            session_dir,
            profiles,
            cfg,
            source,
            temp_key,
            ui_tx.clone(),
            &mut cmd_rx,
        )
        .await;
        match res {
            Ok((conv_count, msg_count)) => {
                let _ = ui_tx.unbounded_send(SyncUiEvent::Completed {
                    conv_count,
                    msg_count,
                });
            }
            Err(SyncRunAbort::Cancelled) => {
                let _ = ui_tx.unbounded_send(SyncUiEvent::Cancelled);
            }
            Err(SyncRunAbort::Failed { message, can_retry }) => {
                let _ = ui_tx.unbounded_send(SyncUiEvent::Error { message, can_retry });
            }
        }
    });

    cmd_tx
}

enum SyncRunAbort {
    Cancelled,
    Failed { message: String, can_retry: bool },
}

async fn run_sync_job(
    session_dir: PathBuf,
    profiles: Profiles,
    cfg: SyncRemoteConfig,
    source: SyncSource,
    temp_key: String,
    ui_tx: futures_mpsc::UnboundedSender<SyncUiEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<SyncWorkerCmd>,
) -> Result<(usize, usize), SyncRunAbort> {
    if !WsRpcHub::global().is_online() {
        return Err(SyncRunAbort::Failed {
            message: "Không có kết nối máy chủ tin nhắn. Vui lòng thử lại sau.".into(),
            can_retry: true,
        });
    }

    let wasm_path = locate_wasm(SYNC_WASM_FILENAME).ok_or_else(|| SyncRunAbort::Failed {
        message: format!("Không tìm thấy tệp mã hóa Sync 2 ({SYNC_WASM_FILENAME})"),
        can_retry: false,
    })?;
    let wasm = Arc::new(Mutex::new(
        SyncWasm::from_file(&wasm_path).map_err(|e| SyncRunAbort::Failed {
            message: format!("Lỗi khởi tạo Sync 2 WASM: {e}"),
            can_retry: false,
        })?,
    ));
    let ik = get_or_generate_ik(&wasm).map_err(|e| SyncRunAbort::Failed {
        message: e,
        can_retry: false,
    })?;

    let device = Device::load_or_create(&session_dir).map_err(|e| SyncRunAbort::Failed {
        message: e.to_string(),
        can_retry: true,
    })?;
    let session = StoredSession::load(&session_dir).map_err(|e| SyncRunAbort::Failed {
        message: e.to_string(),
        can_retry: false,
    })?;
    let device_name = device.computer_name.clone();
    let api = Arc::new(ZaloApi::from_session(device, &session).map_err(|e| {
        SyncRunAbort::Failed {
            message: e.to_string(),
            can_retry: true,
        }
    })?);
    let (_client_id, self_uid) = WsRpcHub::global().session_ids();
    let transport = Arc::new(GpuiSyncTransport {
        api: Arc::clone(&api),
    });

    let store_path = session_dir.join("store.db");
    let ussidx = Arc::new(UssIdxCounter::default());
    let ussid = format!("uss_{}", unix_now_ms());
    let opts = SessionOptions::from_timeouts(cfg.timeout_configs(source), cfg.transfer_batch_size);

    let conv_mgr = SessionManager::new(
        ik.clone(),
        temp_key.clone(),
        ussid.clone(),
        device_name.clone(),
        opts,
        Arc::clone(&ussidx),
    );
    let msg_mgr = SessionManager::new(
        ik,
        temp_key.clone(),
        ussid,
        device_name,
        opts,
        Arc::clone(&ussidx),
    );

    let conv_db = Arc::new(ConversationRemoteDb::new(conv_mgr));
    let msg_db = Arc::new(MessageRemoteDb::new(msg_mgr));

    // Subscribe to control pushes BEFORE issuing any 590!
    let mut ctrl_rx = WsRpcHub::global().subscribe_sync_controls();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let mobile_fail = Arc::new(Mutex::new(None::<SyncFailReason>));

    // Manual sync starts in WaitingConfirm until mobile confirms or sends batches.
    if source == SyncSource::Manual && temp_key.is_empty() {
        let _ = ui_tx.unbounded_send(SyncUiEvent::WaitingConfirm);
    } else {
        let _ = ui_tx.unbounded_send(SyncUiEvent::Progress { percent: None });
    }

    // Spawn control-router task for the lifetime of this sync run.
    let conv_db_ctrl = Arc::clone(&conv_db);
    let msg_db_ctrl = Arc::clone(&msg_db);
    let tr_ctrl = Arc::clone(&transport);
    let ui_ctrl = ui_tx.clone();
    let cancel_ctrl = Arc::clone(&cancel_flag);
    let fail_ctrl = Arc::clone(&mobile_fail);
    let (stop_router_tx, mut stop_router_rx) = tokio::sync::oneshot::channel::<()>();

    let router_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop_router_rx => break,
                ev = ctrl_rx.recv() => {
                    let Ok(ev) = ev else { continue };
                    match ev {
                        SyncControlEvent::MobileStatus(st) => {
                            let matched_conv = conv_db_ctrl.manager().dispatch_mobile_status(&st).await;
                            let matched_msg = if matched_conv {
                                false
                            } else {
                                msg_db_ctrl.manager().dispatch_mobile_status(&st).await
                            };
                            if matched_conv || matched_msg {
                                match classify_mobile_status(st.status) {
                                    MobileStatusDisposition::UiWaitingConfirm => {
                                        let _ = ui_ctrl.unbounded_send(SyncUiEvent::WaitingConfirm);
                                    }
                                    MobileStatusDisposition::UiConfirmed
                                    | MobileStatusDisposition::UiActive
                                    | MobileStatusDisposition::UiIdle => {
                                        let _ = ui_ctrl.unbounded_send(SyncUiEvent::Progress { percent: None });
                                    }
                                    MobileStatusDisposition::Cancel(reason) => {
                                        cancel_ctrl.store(true, Ordering::SeqCst);
                                        conv_db_ctrl.close(tr_ctrl.as_ref(), reason).await;
                                        msg_db_ctrl.close(tr_ctrl.as_ref(), reason).await;
                                    }
                                    MobileStatusDisposition::Fail(fail_reason, dispose) => {
                                        *fail_ctrl.lock().unwrap() = Some(fail_reason);
                                        cancel_ctrl.store(true, Ordering::SeqCst);
                                        conv_db_ctrl.close(tr_ctrl.as_ref(), dispose).await;
                                        msg_db_ctrl.close(tr_ctrl.as_ref(), dispose).await;
                                    }
                                }
                            }
                        }
                        SyncControlEvent::UploadBatch(b) | SyncControlEvent::BatchError(b) => {
                            if !conv_db_ctrl.manager().dispatch_batch(b.clone()).await {
                                let _ = msg_db_ctrl.manager().dispatch_batch(b).await;
                            }
                        }
                        SyncControlEvent::TransferAfterLogin(_) => {}
                    }
                }
            }
        }
    });

    // Forward UI cancel / wakeup commands concurrently.
    let conv_db_cmd = Arc::clone(&conv_db);
    let msg_db_cmd = Arc::clone(&msg_db);
    let tr_cmd = Arc::clone(&transport);
    let cancel_cmd = Arc::clone(&cancel_flag);
    let cmd_drain = async {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                SyncWorkerCmd::Wakeup => {
                    let _ = conv_db_cmd.manager().wakeup(tr_cmd.as_ref()).await;
                    let _ = msg_db_cmd.manager().wakeup(tr_cmd.as_ref()).await;
                }
                SyncWorkerCmd::Cancel => {
                    cancel_cmd.store(true, Ordering::SeqCst);
                    conv_db_cmd
                        .close(tr_cmd.as_ref(), DisposeReason::UserCancel)
                        .await;
                    msg_db_cmd
                        .close(tr_cmd.as_ref(), DisposeReason::UserCancel)
                        .await;
                    break;
                }
            }
        }
    };

    // Main Sync 2 pipeline: Step 1 Conversations -> Step 2 Messages (`sync_other_messages_macro`).
    let sync_main = async {
        let mut all_convs: Vec<SyncConversation> = Vec::new();
        let conv_out = sync_conversations_macro(
            &conv_db,
            transport.as_ref(),
            wasm.as_ref(),
            &cfg,
            source,
            TimeRange::default(),
            0,
            &cancel_flag,
            |page, _| {
                all_convs.extend(page.iter().cloned());
            },
        )
        .await;

        if let Err(e) = conv_out {
            conv_db
                .close(transport.as_ref(), DisposeReason::Error)
                .await;
            return Err(e);
        }

        // Resolve global conversation IDs -> PC thread IDs and upsert threads in ONE transaction.
        let global_pairs: Vec<(String, bool)> = all_convs
            .iter()
            .map(|c| (c.conv_id.clone(), c.conv_type == ConvType::Group))
            .collect();
        let g2pc = resolve_global_to_pc(&api, &profiles, &global_pairs).await;

        let thread_records: Vec<ThreadRecord> = all_convs
            .iter()
            .filter_map(|conv| {
                let pc_tid = g2pc.get(&conv.conv_id)?;
                let is_group = conv.conv_type == ConvType::Group;
                let title = profiles
                    .map
                    .get(pc_tid)
                    .map(|p| p.name.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| pc_tid.clone());
                Some(ThreadRecord {
                    thread_id: pc_tid.clone(),
                    kind: if is_group {
                        ThreadKind::Group
                    } else {
                        ThreadKind::User
                    },
                    title,
                    peer_user_id: (!is_group).then(|| pc_tid.clone()),
                    unread: 0,
                    updated_at: conv.last_ts as i64,
                })
            })
            .collect();
        if !thread_records.is_empty() {
            let store_path_threads = store_path.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(mut store) = LocalStore::open(&store_path_threads) {
                    let _ = store.upsert_threads_batch(&thread_records);
                }
            })
            .await;
            let _ = ui_tx.unbounded_send(SyncUiEvent::DataUpdated);
        }

        // Build MissingMessageRange list for Step 2 (`sync_other_messages_macro`).
        let ranges: Vec<MissingMessageRange> = all_convs
            .iter()
            .filter_map(|c| {
                let pc_tid = g2pc.get(&c.conv_id)?.clone();
                let is_group = c.conv_type == ConvType::Group;
                Some(MissingMessageRange {
                    conv_id: pc_tid,
                    global_noised_id: c.conv_id.clone(),
                    is_group,
                    from_ts: 0,
                    to_ts: JS_MAX_SAFE_INTEGER,
                })
            })
            .collect();

        if ranges.is_empty() {
            return Ok((all_convs.len(), 0usize));
        }

        // Dedicated serialized SQLite writer thread: opens `LocalStore` ONCE, commits each batch
        // in a single transaction (no per-message fsync storm or SQLITE_BUSY contention), never
        // blocks Tokio worker threads, and throttles `DataUpdated` UI refreshes to <= 1 per 2s.
        let (db_tx, db_rx) = std::sync::mpsc::channel::<Vec<(MessageRecord, Option<String>)>>();
        let store_path_writer = store_path.clone();
        let ui_data = ui_tx.clone();
        let writer_handle = std::thread::Builder::new()
            .name("zalo-sync-db-writer".into())
            .spawn(move || -> usize {
                let Ok(mut store) = LocalStore::open(&store_path_writer) else {
                    return 0;
                };
                let mut total_inserted = 0usize;
                let mut pending_notify = false;
                let mut last_notify = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(5))
                    .unwrap_or_else(std::time::Instant::now);
                while let Ok(batch) = db_rx.recv() {
                    if let Ok(n) = store.insert_messages_batch(&batch) {
                        if n > 0 {
                            total_inserted += n;
                            pending_notify = true;
                        }
                    }
                    if pending_notify
                        && last_notify.elapsed() >= std::time::Duration::from_secs(2)
                    {
                        let _ = ui_data.unbounded_send(SyncUiEvent::DataUpdated);
                        pending_notify = false;
                        last_notify = std::time::Instant::now();
                    }
                }
                if pending_notify {
                    let _ = ui_data.unbounded_send(SyncUiEvent::DataUpdated);
                }
                total_inserted
            })
            .ok();

        let g2pc_arc = Arc::new(g2pc);
        let g2pc_cb = Arc::clone(&g2pc_arc);
        let self_uid_cb = self_uid.clone();
        let ui_prog = ui_tx.clone();
        let db_tx_cb = Mutex::new(db_tx);

        let on_messages = Arc::new(move |range: &MissingMessageRange, msgs: Vec<SyncMessage>| {
            let items = build_synced_message_records(
                &range.conv_id,
                range.is_group,
                &self_uid_cb,
                &g2pc_cb,
                &msgs,
            );
            if !items.is_empty() {
                if let Ok(tx) = db_tx_cb.lock() {
                    let _ = tx.send(items);
                }
            }
        });

        let on_progress = Arc::new(
            move |_completed: usize, _total: usize, percent: Option<u8>, _msgs: usize| {
                let _ = ui_prog.unbounded_send(SyncUiEvent::Progress { percent });
            },
        );

        let msg_res = sync_other_messages_macro(
            Arc::clone(&msg_db),
            Arc::clone(&transport),
            Arc::clone(&wasm),
            &cfg,
            ranges,
            Arc::clone(&cancel_flag),
            on_messages.clone(),
            on_progress,
        )
        .await;

        // Drop the callback (and its `db_tx` sender) so the SQLite writer drains and exits cleanly.
        drop(on_messages);
        let saved = if let Some(handle) = writer_handle {
            tokio::task::spawn_blocking(move || handle.join().unwrap_or(0))
                .await
                .unwrap_or(0)
        } else {
            0
        };

        // Also sync "My Documents" (`send2me_id`) from Zalo's cloud message server
        // (`/api/cm/getrecentv2` + `/api/cm/getoldv2` with `isOA: 1`), since mobile Sync 2
        // does not include My Documents.
        let my_docs_saved = if !cancel_flag.load(Ordering::SeqCst) && !self_uid.is_empty() {
            let docs =
                crate::realtime::sync_my_documents_cloud(&api, &store_path, &self_uid, 50, true)
                    .await;
            let n = docs.len();
            if n > 0 {
                let _ = ui_tx.unbounded_send(SyncUiEvent::DataUpdated);
            }
            n
        } else {
            0
        };

        let reason = if cancel_flag.load(Ordering::SeqCst) {
            DisposeReason::UserCancel
        } else if msg_res.is_err() {
            DisposeReason::Error
        } else {
            DisposeReason::SyncSuccess
        };
        msg_db.close(transport.as_ref(), reason).await;

        if let Err(fail) = msg_res {
            return Err(SyncSessionError::Remote {
                code: -1,
                message: format!("{fail:?}"),
            });
        }

        Ok((all_convs.len(), saved + my_docs_saved))
    };

    tokio::pin!(cmd_drain);
    let outcome = tokio::select! {
        res = sync_main => res,
        _ = &mut cmd_drain => Err(SyncSessionError::Released),
    };

    let _ = stop_router_tx.send(());
    let _ = router_handle.await;

    if let Some(fail) = mobile_fail.lock().unwrap().take() {
        let message = match fail {
            SyncFailReason::MobileUserReject => "Điện thoại đã từ chối yêu cầu đồng bộ tin nhắn.".into(),
            SyncFailReason::MobileBusy => "Điện thoại đang bận. Vui lòng thử lại sau.".into(),
            SyncFailReason::MobileLowStorage => "Điện thoại không đủ dung lượng trống để đồng bộ.".into(),
            SyncFailReason::MobileLogout => "Tài khoản trên điện thoại đã đăng xuất.".into(),
            other => format!("Đồng bộ tin nhắn bị dừng: {other:?}"),
        };
        return Err(SyncRunAbort::Failed {
            message,
            can_retry: true,
        });
    }
    if cancel_flag.load(Ordering::SeqCst) {
        return Err(SyncRunAbort::Cancelled);
    }

    match outcome {
        Ok(counts) => Ok(counts),
        Err(SyncSessionError::Released) => Err(SyncRunAbort::Cancelled),
        Err(err) => Err(SyncRunAbort::Failed {
            message: format!("Đồng bộ tin nhắn gặp lỗi: {err}"),
            can_retry: true,
        }),
    }
}

/// Converts one range's decrypted [`SyncMessage`]s into `(MessageRecord, Option<attachment_json>)`
/// pairs ready for single-transaction bulk insertion on the dedicated SQLite writer thread.
fn build_synced_message_records(
    thread_id: &str,
    is_group: bool,
    self_uid: &str,
    g2pc: &HashMap<String, String>,
    msgs: &[SyncMessage],
) -> Vec<(MessageRecord, Option<String>)> {
    let mut out = Vec::with_capacity(msgs.len());
    for msg in msgs {
        let _ = is_group;
        let server_json = sync_message_to_server_json(msg, thread_id, self_uid, |gid| {
            if gid == "0" || gid.is_empty() {
                self_uid.to_owned()
            } else {
                g2pc.get(gid).cloned().unwrap_or_else(|| gid.to_owned())
            }
        });
        let global_id_i64 = msg.global_id.parse::<i64>().ok().filter(|&n| n > 0);
        let sender_pc = if msg.sender_id == "0" || msg.sender_id.is_empty() {
            Some(self_uid.to_owned())
        } else {
            Some(
                g2pc.get(&msg.sender_id)
                    .cloned()
                    .unwrap_or_else(|| msg.sender_id.clone()),
            )
        };
        let body = if !msg.content.is_empty() {
            msg.content.clone()
        } else if let Some(ref att) = msg.meta.as_ref().and_then(|m| m.attachs.first()) {
            if let Some(t) = att.title.as_deref().filter(|s| !s.is_empty()) {
                t.to_owned()
            } else if let Some(d) = att.description.as_deref().filter(|s| !s.is_empty()) {
                d.to_owned()
            } else {
                let kind = server_json
                    .as_ref()
                    .and_then(|v| v.get("msgType"))
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                format!("[{kind}]")
            }
        } else {
            String::new()
        };
        let rec = MessageRecord {
            message_id: 0,
            thread_id: thread_id.to_owned(),
            sender_id: sender_pc,
            cli_msg_id: (msg.client_id > 0).then_some(msg.client_id as i64),
            global_msg_id: global_id_i64,
            body,
            sent_at: msg.timestamp as i64,
        };
        let attach = if msg.meta.is_some() {
            server_json.as_ref().map(|v| v.to_string())
        } else {
            None
        };
        out.push((rec, attach));
    }
    out
}

impl ShellView {
    /// Starts a manual Phone → PC Sync 2 session (from Settings → Đồng bộ và sao lưu).
    pub(crate) fn start_manual_sync(&mut self, cx: &mut Context<Self>) {
        if self.sync_ui.is_running() || self.backup_ui.is_running() {
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
        self.launch_sync_session(SyncSource::Manual, String::new(), cx);
    }

    /// Starts an automatic post-login Sync 2 session when mobile pushes `transfer_after_login`.
    pub(crate) fn handle_transfer_after_login(
        &mut self,
        payload: TransferAfterLoginEvent,
        cx: &mut Context<Self>,
    ) {
        if !self.sync_ui.config.is_enabled_for(SyncSource::AutoSync) {
            return;
        }
        if self.sync_ui.is_running() || self.backup_ui.is_running() {
            return;
        }
        self.launch_sync_session(SyncSource::AutoSync, payload.temp_key, cx);
    }

    fn launch_sync_session(
        &mut self,
        source: SyncSource,
        temp_key: String,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.session_dir.clone() else {
            self.set_note("Chưa đăng nhập tài khoản để đồng bộ tin nhắn");
            cx.notify();
            return;
        };
        let (ui_tx, mut ui_rx) = futures_mpsc::unbounded();
        let cmd_tx = spawn_sync_job(
            dir,
            self.profiles.clone(),
            self.sync_ui.config.clone(),
            source,
            temp_key,
            ui_tx,
        );
        self.sync_ui.running = true;
        self.sync_ui.cmd_tx = Some(cmd_tx);
        self.sync_ui.banner = if source == SyncSource::Manual {
            SyncBannerState::WaitingConfirm
        } else {
            SyncBannerState::Syncing { percent: None }
        };
        cx.notify();

        let this = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            use futures::StreamExt;
            while let Some(ev) = ui_rx.next().await {
                if this
                    .update(cx, |view, cx| {
                        match ev {
                            SyncUiEvent::WaitingConfirm => {
                                if !matches!(
                                    view.sync_ui.banner,
                                    SyncBannerState::Syncing { percent: Some(_) }
                                ) {
                                    view.sync_ui.banner = SyncBannerState::WaitingConfirm;
                                }
                            }
                            SyncUiEvent::Progress { percent } => {
                                let next_percent = match (&view.sync_ui.banner, percent) {
                                    (SyncBannerState::Syncing { percent: Some(prev) }, None) => {
                                        Some(*prev)
                                    }
                                    (SyncBannerState::Syncing { percent: Some(prev) }, Some(cur)) => {
                                        Some((*prev).max(cur))
                                    }
                                    (_, other) => other,
                                };
                                view.sync_ui.banner = SyncBannerState::Syncing {
                                    percent: next_percent,
                                };
                            }
                            SyncUiEvent::DataUpdated => {
                                view.reload_from_store();
                            }
                            SyncUiEvent::Completed {
                                conv_count,
                                msg_count,
                            } => {
                                view.sync_ui.running = false;
                                view.sync_ui.cmd_tx = None;
                                view.sync_ui.banner = SyncBannerState::Completed {
                                    conv_count,
                                    msg_count,
                                };
                                view.reload_from_store();
                            }
                            SyncUiEvent::Cancelled => {
                                view.sync_ui.running = false;
                                view.sync_ui.cmd_tx = None;
                                view.sync_ui.banner = SyncBannerState::Hidden;
                            }
                            SyncUiEvent::Error { message, can_retry } => {
                                view.sync_ui.running = false;
                                view.sync_ui.cmd_tx = None;
                                view.sync_ui.banner = SyncBannerState::Error { message, can_retry };
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

    /// Sidebar Sync 2 status banner rendered below `list_tabs` and above `conv_rows`.
    pub(crate) fn sync_banner_el(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let dark = crate::color::is_dark();
        let bg = if dark { 0x1e_29_3b } else { 0xe8_f2_ff };
        let err_bg = if dark { 0x3b_1f_22 } else { 0xfd_ed_ed };
        let ok_bg = if dark { 0x17_33_26 } else { 0xe8_f7_ee };
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let border = if dark { 0x33_41_55 } else { 0xc7_dd_ff };

        match &self.sync_ui.banner {
            SyncBannerState::Hidden => None,
            SyncBannerState::WaitingConfirm => Some(
                div()
                    .id("sync2-banner-wait")
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
                            .child("Đang chờ xác nhận trên điện thoại"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(muted))
                            .child("Mở Zalo trên điện thoại và bấm Đồng bộ ngay."),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("sync2-wakeup-btn")
                                    .px(px(10.))
                                    .py(px(4.))
                                    .rounded(px(4.))
                                    .bg(rgb(blue))
                                    .text_size(px(12.))
                                    .text_color(rgb(0xff_ff_ff))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sync_ui.wakeup();
                                        this.set_note("Đã gửi lại yêu cầu xác nhận tới điện thoại");
                                        cx.notify();
                                    }))
                                    .child("Gửi lại yêu cầu"),
                            )
                            .child(
                                div()
                                    .id("sync2-cancel-btn")
                                    .px(px(10.))
                                    .py(px(4.))
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(rgb(border))
                                    .text_size(px(12.))
                                    .text_color(rgb(ink))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sync_ui.cancel();
                                        cx.notify();
                                    }))
                                    .child("Hủy"),
                            ),
                    )
                    .into_any_element(),
            ),
            SyncBannerState::Syncing { percent } => {
                let title = match percent {
                    Some(p) => format!("Đang đồng bộ tin nhắn ({}%)", (*p).min(100)),
                    None => "Đang đồng bộ tin nhắn…".to_owned(),
                };
                let bar_pct = percent.unwrap_or(0).min(100);
                Some(
                    div()
                        .id("sync2-banner-progress")
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
                                .flex()
                                .flex_row()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(ink))
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .id("sync2-stop-btn")
                                        .text_size(px(12.))
                                        .text_color(rgb(blue))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sync_ui.cancel();
                                            cx.notify();
                                        }))
                                        .child("Dừng"),
                                ),
                        )
                        .when(percent.is_some(), |el| {
                            el.child(
                                div()
                                    .w_full()
                                    .h(px(4.))
                                    .rounded(px(2.))
                                    .bg(rgb(border))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(gpui::relative(bar_pct as f32 / 100.0))
                                            .rounded(px(2.))
                                            .bg(rgb(blue)),
                                    ),
                            )
                        })
                        .into_any_element(),
                )
            }
            SyncBannerState::Completed {
                conv_count,
                msg_count,
            } => Some(
                div()
                    .id("sync2-banner-done")
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
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(ink))
                                    .child("Đồng bộ tin nhắn hoàn tất"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(muted))
                                    .child(format!(
                                        "{conv_count} hội thoại, {msg_count} tin nhắn mới"
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("sync2-done-close")
                            .text_size(px(12.))
                            .text_color(rgb(blue))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sync_ui.banner = SyncBannerState::Hidden;
                                cx.notify();
                            }))
                            .child("Đóng"),
                    )
                    .into_any_element(),
            ),
            SyncBannerState::Error { message, can_retry } => {
                let msg_text = message.clone();
                let retryable = *can_retry;
                Some(
                    div()
                        .id("sync2-banner-err")
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
                                            .id("sync2-retry-btn")
                                            .px(px(10.))
                                            .py(px(4.))
                                            .rounded(px(4.))
                                            .bg(rgb(blue))
                                            .text_size(px(12.))
                                            .text_color(rgb(0xff_ff_ff))
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.start_manual_sync(cx);
                                            }))
                                            .child("Thử lại"),
                                    )
                                })
                                .child(
                                    div()
                                        .id("sync2-err-close")
                                        .px(px(10.))
                                        .py(px(4.))
                                        .rounded(px(4.))
                                        .text_size(px(12.))
                                        .text_color(rgb(muted))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sync_ui.banner = SyncBannerState::Hidden;
                                            cx.notify();
                                        }))
                                        .child("Đóng"),
                                ),
                        )
                        .into_any_element(),
                )
            }
        }
    }
}
