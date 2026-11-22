//! Realtime messages over `zpw_ws`, ported from `zalo-gui`'s `realtime_worker`.
//!
//! One background thread: `getLoginInfo` (retry with backoff) →
//! `getServerInfo` → `run_listener` from the newest message ids already in
//! `store.db`, so messages missed while closed arrive as history
//! (`cmd 510/511`). Parsed messages are written to `store.db` in the same
//! shape `zalo-gui` uses and sent to the view as [`RtEvent`]s.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use serde_json::Value;
use zalo_client::{run_listener_with, Device, ListenerEvent, StoredSession, ZaloApi};
use zalo_storage::{LocalStore, MessageRecord, ThreadKind, ThreadRecord, UserRecord};

use crate::media::{MsgKind, PollChoice};

/// What the view needs to know.
#[derive(Clone, Debug)]
pub enum RtEvent {
    /// Socket ready.
    Online,
    /// Dropped / retrying, with the reason.
    Offline(String),
    /// Another web client took the session.
    Replaced,
    /// New messages (`live` false for gap-fill history).
    Messages(Vec<Incoming>, bool),
    /// `cmd 601` controls with `act_type: "voip"` (calls).
    Voip(Vec<serde_json::Value>),
    /// Read/seen marks (`clearUnreads`, `seens`, `groupSeens`, `delivereds`).
    Status(zalo_client::StatusUpdate),
    /// Peer typing events (`cmd 602`).
    Typing(Vec<zalo_client::listener::Typing>),
    /// Message reactions (`cmd 610/611/612/613`).
    Reactions(Vec<ReactionUpdate>),
    /// Synced pinned conversation ids from `/api/pinconvers/list`.
    Pinned(Vec<String>),
    /// Synced archived conversation ids from `/api/archivedchat/list`.
    Archived(Vec<String>),
    /// Remote feature configs parsed from `getServerInfo` (`sync_2_pc` & `backup_msg`).
    RemoteConfigs {
        /// Sync 2 (`sync_2_pc`) remote config.
        sync: zalo_sync::SyncRemoteConfig,
        /// PC → Phone Backup (`backup_msg`) remote config.
        backup: zalo_backup::BackupRemoteConfig,
    },
    /// Automatic Sync 2 trigger pushed by mobile right after login (`transfer_after_login`).
    SyncAfterLogin(zalo_sync::TransferAfterLoginEvent),
}

/// One parsed reaction update from `610/611/612/613`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionUpdate {
    /// `<uid>` or `g_<grid>`.
    pub thread_id: String,
    /// Group thread.
    pub is_group: bool,
    /// Target server `msgId` (`gMsgID`).
    pub target_global_id: Option<i64>,
    /// Target client `cliMsgId` (`cMsgID`).
    pub target_cli_id: Option<i64>,
    /// Sent by this account (`uidFrom == "0"` or `self_uin`).
    pub from_me: bool,
    /// Reaction author uid.
    pub sender_uid: String,
    /// Raw reaction token (`"/-strong"`, `"/-heart"`, …).
    pub r_icon: String,
    /// Numeric reaction type (`-1` removes).
    pub r_type: i64,
    /// `true` when `cmd == 613` or `r_type < 0`.
    pub cleared: bool,
}

/// One parsed message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Incoming {
    /// `<uid>` or `g_<grid>`.
    pub thread_id: String,
    /// Group thread.
    pub is_group: bool,
    /// Sent by this account (another device).
    pub from_me: bool,
    /// Sender uid (empty when from me).
    pub sender_uid: String,
    /// `dName` of the sender.
    pub sender_name: String,
    /// Stored body (image placeholder, `[File] name`, text…).
    pub text: String,
    /// Kind for the bubble.
    pub kind: MsgKind,
    /// Photo / sticker / GIF URL (full-resolution uncropped URL for photos).
    pub media_url: String,
    /// Optional low-res thumbnail URL (kept as fallback in stored attachments).
    pub thumb_url: String,
    /// Declared width.
    pub width: u32,
    /// Declared height.
    pub height: u32,
    /// File name.
    pub file_name: String,
    /// File size.
    pub file_size: u64,
    /// Unix ms.
    pub at_ms: i64,
    /// `cliMsgId`.
    pub cli_msg_id: Option<i64>,
    /// `msgId`.
    pub global_msg_id: Option<i64>,
    /// Voice / video file URL, or the link of a link card.
    pub href: String,
    /// Voice / video length in milliseconds.
    pub duration_ms: u64,
    /// Link card title / description from `chat.recommended`.
    pub link_title: String,
    /// Link card description.
    pub link_desc: String,
    /// Raw `{cmi, gmi, si, di, mt, st, at, cmd, ts}` record for `/seenv2`.
    pub seen_record: Option<Value>,
    /// Quoted message sender (`quote.fromD` or `quote.ownerId`).
    pub quote_sender: String,
    /// Quoted message text/placeholder snippet (`quote.msg`).
    pub quote_text: String,
    /// Quoted server id (`quote.globalMsgId` / `msgId`).
    pub quote_global_id: Option<i64>,
    /// Quoted client id (`quote.cliMsgId`).
    pub quote_cli_id: Option<i64>,
    /// `store.db` row id once written (0 when not stored).
    pub row_id: i64,
    /// `group.poll` id.
    pub poll_id: i64,
    /// Poll question (also copied into `file_name` on the bubble).
    pub poll_question: String,
    /// `create` / `share` / `vote` / `update` / `close`.
    pub poll_action: String,
    /// Poll is closed.
    pub poll_closed: bool,
    /// Choices present on the message. Detail fills the rest.
    pub poll_options: Vec<PollChoice>,
    /// `1` when the row came from `/api/message/fetchImportant`.
    pub urgency: u8,
    /// Album id when this photo is one cell of a multi-photo send. `0` otherwise.
    pub group_layout_id: i64,
}

static FILES: std::sync::OnceLock<(
    std::sync::Mutex<std::collections::HashMap<String, String>>,
    std::sync::Condvar,
)> = std::sync::OnceLock::new();
static ACTIVE_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LISTENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Invalidates the active realtime generation so any winding-down worker stops
/// publishing events or status.
pub(crate) fn invalidate_generation(new_gen: u64) {
    ACTIVE_GEN.store(new_gen, std::sync::atomic::Ordering::SeqCst);
    LISTENING.store(false, std::sync::atomic::Ordering::SeqCst);
}

fn is_active_gen(gen: u64) -> bool {
    ACTIVE_GEN.load(std::sync::atomic::Ordering::SeqCst) == gen
}

fn set_listening_for(gen: u64, on: bool) {
    if is_active_gen(gen) {
        LISTENING.store(on, std::sync::atomic::Ordering::SeqCst);
    }
}

fn files() -> &'static (
    std::sync::Mutex<std::collections::HashMap<String, String>>,
    std::sync::Condvar,
) {
    FILES.get_or_init(Default::default)
}

/// Record a `file_done` control (fileId → url) and wake waiters.
pub fn file_done(id: String, url: String) {
    let (map, cv) = files();
    if let Ok(mut m) = map.lock() {
        m.insert(id, url);
        cv.notify_all();
    }
}

/// True while the socket is up (async file sends need its `file_done`).
pub fn listening() -> bool {
    LISTENING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Block until `file_done` for `id` arrives (or `timeout`).
pub fn wait_file_url(id: &str, timeout: Duration) -> Option<String> {
    let (map, cv) = files();
    let deadline = std::time::Instant::now() + timeout;
    let mut m = map.lock().ok()?;
    loop {
        if let Some(url) = m.remove(id) {
            return Some(url);
        }
        let left = deadline.checked_duration_since(std::time::Instant::now())?;
        m = cv.wait_timeout(m, left).ok()?.0;
    }
}

/// Start the listener thread with a generational shutdown channel.
pub fn start_with_shutdown(
    dir: PathBuf,
    tx: UnboundedSender<RtEvent>,
    gen: u64,
    stop_rx: tokio::sync::watch::Receiver<bool>,
) {
    ACTIVE_GEN.store(gen, std::sync::atomic::Ordering::SeqCst);
    let _ = std::thread::Builder::new()
        .name("zalo-gpui-realtime".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                let _ = tx.unbounded_send(RtEvent::Offline("tokio".into()));
                return;
            };
            rt.block_on(run(&dir, &tx, gen, stop_rx));
            set_listening_for(gen, false);
        });
}

/// Start the listener thread. It stops when the receiver is dropped.
pub fn start(dir: PathBuf, tx: UnboundedSender<RtEvent>) {
    let gen = ACTIVE_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    start_with_shutdown(dir, tx, gen, stop_rx);
}

async fn wait_stopped(stop_rx: &mut tokio::sync::watch::Receiver<bool>) {
    if *stop_rx.borrow() {
        return;
    }
    while stop_rx.changed().await.is_ok() {
        if *stop_rx.borrow() {
            return;
        }
    }
}

fn is_missing_session_err(err: &zalo_client::SessionError) -> bool {
    matches!(err, zalo_client::SessionError::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound)
}

async fn run(
    dir: &Path,
    tx: &UnboundedSender<RtEvent>,
    gen: u64,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
) {
    let store_path = dir.join("store.db");
    let mut synced = false;
    let mut refresh_wait = Duration::from_secs(2);
    loop {
        if tx.is_closed() || *stop_rx.borrow() || !is_active_gen(gen) {
            return;
        }
        let mut wait = Duration::from_secs(5);
        let (api, server_info, pc_server_info, login_settings, self_uin, device_imei) = loop {
            if tx.is_closed() || *stop_rx.borrow() || !is_active_gen(gen) {
                return;
            }
            let attempt = async {
                let device = Device::load_or_create(dir).map_err(|e| (e.to_string(), false))?;
                let imei = device.imei.clone();
                let mut session = match StoredSession::load(dir) {
                    Ok(s) => s,
                    Err(err) => {
                        let missing = is_missing_session_err(&err);
                        return Err((err.to_string(), missing));
                    }
                };
                let mut api = ZaloApi::from_session(device, &session)
                    .map_err(|e| (e.to_string(), false))?;
                let info = api
                    .get_login_info()
                    .await
                    .map_err(|e| (e.to_string(), false))?;
                // Persist refreshed cookies and keys back to session.json so
                // `bridge::spawn_live` does not keep loading stale cookies.
                let fresh_cookies = api.cookie_headers();
                if !fresh_cookies.is_empty() {
                    session.cookie_headers = fresh_cookies;
                    if let Some(sek) = zalo_client::session::extract_zpw_sek(&session.cookie_headers) {
                        session.zpw_sek = Some(sek);
                    }
                }
                session.apply_login_info(
                    info.uin.clone(),
                    info.zpw_enk.clone(),
                    info.phone_number.clone().or(session.phone_number.clone()),
                    info.display_name.clone().or(session.display_name.clone()),
                );
                if let Some(uid) = info.send2me_id.clone().filter(|s| !s.is_empty()) {
                    session.send2me_id = Some(uid);
                }
                if is_active_gen(gen) && dir.join("session.json").is_file() {
                    let _ = session.save(dir);
                }
                crate::bridge::remember_login_info(
                    dir,
                    session.zpw_sek().as_deref().unwrap_or(""),
                    &info,
                );
                let pc_type = ZaloApi::pc_api_type();
                let (server, pc_server) = if zalo_protocol::api_type() != pc_type {
                    let (s, pc) = tokio::join!(
                        api.get_server_info(),
                        api.get_server_info_for_type(pc_type),
                    );
                    (s.ok(), pc.ok())
                } else {
                    (api.get_server_info().await.ok(), None)
                };
                let login_settings = info.settings.clone();
                let self_uid = info
                    .send2me_id
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| info.uin.clone());
                Ok((api, server, pc_server, login_settings, self_uid, imei))
            };
            let outcome = tokio::select! {
                _ = wait_stopped(&mut stop_rx) => return,
                res = attempt => res,
            };
            match outcome {
                Ok(ready) => break ready,
                Err((err, missing_session)) => {
                    if !is_active_gen(gen) || *stop_rx.borrow() {
                        return;
                    }
                    let fatal = missing_session || crate::login_flow::is_session_error(&err);
                    let _ = tx.unbounded_send(RtEvent::Offline(err));
                    if fatal {
                        return;
                    }
                    tokio::select! {
                        _ = wait_stopped(&mut stop_rx) => return,
                        _ = tokio::time::sleep(wait) => {}
                    }
                    wait = (wait * 2).min(Duration::from_secs(60));
                }
            }
        };
        if !is_active_gen(gen) || *stop_rx.borrow() {
            return;
        }
        let (since_user, since_group) = LocalStore::open(&store_path)
            .ok()
            .and_then(|s| s.newest_global_ids().ok())
            .unwrap_or((None, None));
        let mut config = api.listener_config(server_info.as_ref());
        config.since_user = since_user.map(|n| n.to_string());
        config.since_group = since_group.map(|n| n.to_string());
        config.cursor_path = Some(dir.join("queue-cursors.json"));
        config.max_unready_retries = Some(3);
        let mut sync_cfg = zalo_sync::SyncRemoteConfig::default();
        let mut backup_cfg = zalo_backup::BackupRemoteConfig::default();
        if let Some(ref pc_srv) = pc_server_info {
            sync_cfg.apply_server_info(pc_srv);
            backup_cfg.apply_server_info(pc_srv);
        }
        if let Some(ref srv) = server_info {
            sync_cfg.apply_server_info(srv);
            backup_cfg.apply_server_info(srv);
        }
        if let Some(ref settings) = login_settings {
            sync_cfg.apply_server_info(settings);
            backup_cfg.apply_server_info(settings);
        }
        crate::ws_rpc::WsRpcHub::global().set_remote_configs(sync_cfg.clone(), backup_cfg.clone());
        let _ = tx.unbounded_send(RtEvent::RemoteConfigs {
            sync: sync_cfg,
            backup: backup_cfg,
        });
        let ws_out_rx = crate::ws_rpc::WsRpcHub::global().attach_connection(
            gen,
            device_imei.clone(),
            self_uin.clone(),
        );
        if !synced {
            synced = true;
            tokio::spawn(startup_sync(
                api,
                store_path.clone(),
                self_uin.clone(),
                tx.clone(),
            ));
        }
        let tx2 = tx.clone();
        let store_path_cb = store_path.clone();
        let reached_ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reached_ready_cb = std::sync::Arc::clone(&reached_ready);
        let listen_fut = run_listener_with(config, Some(ws_out_rx), move |event| {
            if tx2.is_closed() || !is_active_gen(gen) {
                return false;
            }
            let out = match event {
                ListenerEvent::Ready => {
                    reached_ready_cb.store(true, std::sync::atomic::Ordering::Relaxed);
                    set_listening_for(gen, true);
                    crate::ws_rpc::WsRpcHub::global().set_online(true);
                    RtEvent::Online
                }
                ListenerEvent::Disconnected(why) => {
                    set_listening_for(gen, false);
                    crate::ws_rpc::WsRpcHub::global().set_online(false);
                    RtEvent::Offline(why)
                }
                ListenerEvent::Replaced => {
                    crate::ws_rpc::WsRpcHub::global().set_online(false);
                    RtEvent::Replaced
                }
                ListenerEvent::Status(status) => {
                    if status.is_empty() {
                        return true;
                    }
                    RtEvent::Status(status)
                }
                ListenerEvent::Typing(list) => {
                    let filtered: Vec<_> = list
                        .into_iter()
                        .filter(|t| !t.user_id.is_empty() && t.user_id != self_uin)
                        .collect();
                    if filtered.is_empty() {
                        return true;
                    }
                    RtEvent::Typing(filtered)
                }
                ListenerEvent::Event { cmd, sub_cmd, data }
                    if cmd == zalo_client::listener::CMD_PUSH_CTRL
                        || cmd == zalo_client::listener::CMD_PULL_CTRL =>
                {
                    if crate::call::call_log_enabled() {
                        eprintln!(
                            "zalo-gpui ws: ctrl cmd={cmd} sub={sub_cmd} {}",
                            crate::call::sanitized_call_json(&data)
                        );
                    }
                    for (id, url) in zalo_client::listener::file_done_from_event(&data) {
                        file_done(id, url);
                    }
                    // Route Sync 2 (`transfer_sync2` / `transfer_msg`) and Backup (`backup_pc2mb`)
                    // control pushes on BOTH 601 and 603 (matching Electron `compact3043.js`).
                    let after_login = crate::ws_rpc::WsRpcHub::global().dispatch_controls(&data);
                    for ev in after_login {
                        if let zalo_sync::SyncControlEvent::TransferAfterLogin(payload) = ev {
                            let _ = tx2.unbounded_send(RtEvent::SyncAfterLogin(payload));
                        }
                    }
                    // 603 is the catch-up of 601. Call confirmations ride on it.
                    // Replaying an old call from that catch-up would ring again.
                    if cmd != zalo_client::listener::CMD_PUSH_CTRL {
                        return true;
                    }
                    let voip = zalo_client::voip_controls_from_event(&data);
                    if voip.is_empty() {
                        return true;
                    }
                    RtEvent::Voip(voip)
                }
                ListenerEvent::Event { cmd, data, .. }
                    if matches!(
                        cmd,
                        zalo_client::listener::CMD_SYNC_REQUEST
                            | zalo_client::listener::CMD_SYNC_ACK_DELETE
                            | zalo_client::listener::CMD_SYNC_WAKEUP
                            | zalo_client::listener::CMD_BACKUP_INIT
                            | zalo_client::listener::CMD_BACKUP_CREATE_SESSION
                            | zalo_client::listener::CMD_BACKUP_GET_METADATA
                            | zalo_client::listener::CMD_BACKUP_SIGNAL_RESTORE
                            | zalo_client::listener::CMD_BACKUP_GET_CONFIGS
                    ) =>
                {
                    let _ = crate::ws_rpc::WsRpcHub::global().complete_rpc(cmd, data);
                    return true;
                }
                ListenerEvent::Event { cmd, data, .. }
                    if matches!(
                        cmd,
                        zalo_client::listener::CMD_PULL_REACT
                            | zalo_client::listener::CMD_PULL_REACT_GROUP
                            | zalo_client::listener::CMD_PUSH_REACT
                            | zalo_client::listener::CMD_CLEAR_REACT
                    ) =>
                {
                    let list = reactions_from_event(cmd, &data, &self_uin);
                    if list.is_empty() {
                        return true;
                    }
                    RtEvent::Reactions(list)
                }
                ListenerEvent::Event { cmd, sub_cmd, data } => {
                    if crate::call::call_log_enabled() {
                        eprintln!(
                            "zalo-gpui ws: event cmd={cmd} sub={sub_cmd} {}",
                            crate::call::sanitized_call_json(&data)
                        );
                    }
                    return true;
                }
                ListenerEvent::Messages {
                    group,
                    items,
                    history,
                    status,
                } => {
                    if !status.is_empty() && tx2.unbounded_send(RtEvent::Status(status)).is_err() {
                        return false;
                    }
                    let mut list: Vec<Incoming> = items
                        .iter()
                        .filter_map(|item| parse_item(item, group, &self_uin))
                        .collect();
                    if list.is_empty() {
                        return true;
                    }
                    store_all(&store_path_cb, &mut list);
                    RtEvent::Messages(list, !history)
                }
                _ => return true,
            };
            tx2.unbounded_send(out).is_ok()
        });
        let result = tokio::select! {
            _ = wait_stopped(&mut stop_rx) => {
                set_listening_for(gen, false);
                crate::ws_rpc::WsRpcHub::global().detach_connection(gen);
                return;
            }
            res = listen_fut => res,
        };
        set_listening_for(gen, false);
        crate::ws_rpc::WsRpcHub::global().detach_connection(gen);
        if !is_active_gen(gen) || *stop_rx.borrow() {
            return;
        }
        if reached_ready.load(std::sync::atomic::Ordering::Relaxed) {
            refresh_wait = Duration::from_secs(2);
        }
        match result {
            Ok(()) => return,
            Err(zalo_client::listener::ListenerError::RefreshNeeded) => {
                eprintln!(
                    "zalo-gpui realtime: consecutive WebSocket drops before Ready; refreshing login info and endpoints in {}s",
                    refresh_wait.as_secs()
                );
                tokio::select! {
                    _ = wait_stopped(&mut stop_rx) => return,
                    _ = tokio::time::sleep(refresh_wait) => {}
                }
                refresh_wait = (refresh_wait * 2).min(Duration::from_secs(60));
            }
            Err(err) => {
                let _ = tx.unbounded_send(RtEvent::Offline(err.to_string()));
                return;
            }
        }
    }
}

/// Fetch server last-messages, pinned threads, and archived threads right after login.
async fn startup_sync(
    api: ZaloApi,
    store_path: PathBuf,
    self_uin: String,
    tx: UnboundedSender<RtEvent>,
) {
    if tx.is_closed() {
        return;
    }
    match api.get_last_messages().await {
        Ok(last) => {
            let status = zalo_client::StatusUpdate::from_event(&last);
            if !status.is_empty() {
                let _ = tx.unbounded_send(RtEvent::Status(status));
            }
            let root = last.get("data").filter(|v| v.is_object()).unwrap_or(&last);
            let mut synced = preload_messages(root, &self_uin);
            if synced.is_empty() {
                if let Some(map) = zero_preload_map(root.get("clearUnreads")) {
                    if let Ok(again) = api.get_last_messages_for(&map).await {
                        let again_root = again
                            .get("data")
                            .filter(|v| v.is_object())
                            .unwrap_or(&again);
                        synced = preload_messages(again_root, &self_uin);
                    }
                }
            }
            eprintln!("zalo-gpui last-msgs parsed {}", synced.len());
            if !synced.is_empty() {
                store_all(&store_path, &mut synced);
                let _ = tx.unbounded_send(RtEvent::Messages(synced, false));
            }
        }
        Err(err) => {
            let kind = err.to_string();
            let kind = kind.split(':').next().unwrap_or("err").trim();
            eprintln!("zalo-gpui last-msgs failed {kind}");
        }
    }
    if !self_uin.is_empty() {
        let my_docs = sync_my_documents_cloud(&api, &store_path, &self_uin, 20, false).await;
        if !my_docs.is_empty() {
            let _ = tx.unbounded_send(RtEvent::Messages(my_docs, false));
        }
    }
    if let Ok(pinned) = api.get_pinned_conversations().await {
        let root = pinned
            .get("data")
            .filter(|v| v.is_object())
            .unwrap_or(&pinned);
        let mut ids: Vec<String> = Vec::new();
        for key in ["conversations", "otherTabConvs"] {
            if let Some(items) = root.get(key).and_then(Value::as_array) {
                for v in items {
                    if let Some(s) = v
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| v.as_i64().map(|n| n.to_string()))
                        .filter(|s| !s.is_empty() && s != "m1")
                    {
                        ids.push(s);
                    }
                }
            }
        }
        if root.get("conversations").is_some() || root.get("otherTabConvs").is_some() {
            let _ = tx.unbounded_send(RtEvent::Pinned(ids));
        }
    }
    if let Ok(archived) = api.get_archived_chats(0).await {
        let (ids, _) = zalo_client::archived_ids_from(&archived);
        let _ = tx.unbounded_send(RtEvent::Archived(ids));
    }
    // Urgent / important inbox. Electron pages this with `lastId` starting at `"-1"`.
    if let Ok(me) = api.get_me().await {
        let uid = crate::account::me_from_value(&me).uid;
        if !uid.is_empty() {
            match api.fetch_important(&uid, None, Some("-1")).await {
                Ok(page) => {
                    let root = important_root(&page);
                    let count = |key: &str| {
                        root.get(key)
                            .and_then(Value::as_array)
                            .map(|items| items.len())
                            .unwrap_or(0)
                    };
                    // Counts only. The page itself holds message bodies.
                    eprintln!(
                        "zalo-gpui important: msgs={} groups={}",
                        count("msgs"),
                        count("groupMsgs")
                    );
                    let mut synced = Vec::new();
                    for (key, group) in [("msgs", false), ("groupMsgs", true)] {
                        if let Some(items) = root.get(key).and_then(Value::as_array) {
                            synced.extend(items.iter().filter_map(|v| {
                                let mut row = parse_item(v, group, &self_uin)?;
                                // This endpoint only returns important rows. Keep a
                                // server `specialMsgType` of 2 (urgent) when set.
                                if row.urgency == 0 {
                                    row.urgency = 1;
                                }
                                Some(row)
                            }));
                        }
                    }
                    if !synced.is_empty() {
                        store_all(&store_path, &mut synced);
                        let _ = tx.unbounded_send(RtEvent::Messages(synced, false));
                    }
                }
                Err(err) => eprintln!("zalo-gpui important: {err}"),
            }
        }
    }
}

/// Inner `{msgs, groupMsgs}` of a decrypted `fetchImportant` reply.
fn important_root(value: &Value) -> Value {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let Some(message) = root.get("message") else {
        return root.clone();
    };
    if message.is_object() {
        return message.clone();
    }
    message
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| root.clone())
}

fn json_str(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn json_i64(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.parse().ok())
}

/// Electron `isGroupPhotoItem`: `is_group_layout` and `total_item_in_group > 1`.
fn photo_group_layout(params: &Value) -> i64 {
    let flag = params
        .get("is_group_layout")
        .or_else(|| params.get("isGroupLayout"));
    let on = match flag {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("true"),
        _ => false,
    };
    if !on {
        return 0;
    }
    let total = params
        .get("total_item_in_group")
        .or_else(|| params.get("totalItemInGroup"))
        .and_then(json_i64)
        .unwrap_or(0);
    if total <= 1 {
        return 0;
    }
    params
        .get("group_layout_id")
        .or_else(|| params.get("groupLayoutId"))
        .and_then(json_i64)
        .filter(|id| *id != 0)
        .unwrap_or(0)
}

fn json_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|n| n as f64))
        .or_else(|| v.as_str()?.parse().ok())
}

fn as_object(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// `zalo-gui` `parse_synced_msg`, reduced to what the GPUI bubbles draw.
#[must_use]
/// `{uid_0: "0", groupId_1: "0"}` so a fresh profile asks for the latest row.
fn zero_preload_map(clear_unreads: Option<&Value>) -> Option<String> {
    let items = clear_unreads?.as_array()?;
    let mut map = serde_json::Map::new();
    for item in items {
        let id = json_str(item, "idTo").unwrap_or_default();
        let id = id.strip_prefix("g_").unwrap_or(&id);
        if id.is_empty() || id == "0" {
            continue;
        }
        let key = if json_flag(item, "isGroup") {
            format!("{id}_1")
        } else {
            format!("{id}_0")
        };
        map.insert(key, Value::String("0".to_owned()));
    }
    (!map.is_empty()).then(|| Value::Object(map).to_string())
}

/// Last-message rows from `msgs` / `groupMsgs`, or Electron's `allmsgs`.
/// Fetches "My Documents" (`send2me_id`) cloud history from `/api/cm/getrecentv2` and
/// `/api/cm/getoldv2` (matching Electron `getCM` with `isOA: 1`), persists it to `store.db`,
/// and returns the parsed messages sorted oldest-first.
///
/// When `full_backfill` is `false` (startup sync) and `store.db` already has local
/// messages for `send2me_id`, paging stops as soon as a page only contains messages
/// already present in `store.db`.
pub(crate) async fn sync_my_documents_cloud(
    api: &ZaloApi,
    store_path: &Path,
    send2me_id: &str,
    max_pages: usize,
    full_backfill: bool,
) -> Vec<Incoming> {
    if send2me_id.is_empty() || max_pages == 0 {
        return Vec::new();
    }
    let existing_gids: std::collections::HashSet<i64> = if full_backfill {
        std::collections::HashSet::new()
    } else {
        let sp = store_path.to_path_buf();
        let sid = send2me_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let Ok(store) = LocalStore::open(&sp) else {
                return std::collections::HashSet::new();
            };
            let msgs = store.messages(&sid).unwrap_or_default();
            let atts: std::collections::HashMap<i64, String> = store
                .attachments()
                .unwrap_or_default()
                .into_iter()
                .collect();
            let needs_photo_upgrade = msgs.iter().any(|m| {
                let Some(raw) = atts.get(&m.message_id) else {
                    return false;
                };
                let Ok(v) = serde_json::from_str::<Value>(raw) else {
                    return false;
                };
                if v.get("kind").and_then(Value::as_str) != Some("photo") {
                    return false;
                }
                if v.get("hd_checked").and_then(Value::as_bool) == Some(true) {
                    return false;
                }
                let thumb = v.get("thumb").and_then(Value::as_str).unwrap_or("");
                let hd = v.get("hd").and_then(Value::as_str).unwrap_or("");
                hd.is_empty()
                    || crate::media::is_thumb_photo_url(hd)
                    || (!thumb.is_empty() && hd == thumb)
            });
            if needs_photo_upgrade {
                return std::collections::HashSet::new();
            }
            msgs.into_iter().filter_map(|m| m.global_msg_id).collect()
        })
        .await
        .unwrap_or_default()
    };
    let mut all: Vec<Incoming> = Vec::new();
    let mut seen_gids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    let mut use_old = false;

    for page in 0..max_pages {
        let resp = if use_old {
            api.get_my_cloud_older(
                send2me_id,
                cursor.as_deref().unwrap_or("9223372036854775808"),
                50,
            )
            .await
        } else {
            api.get_my_cloud_recent(send2me_id, cursor.as_deref(), 50)
                .await
        };
        let Ok(val) = resp else {
            if page == 0 {
                if let Err(err) = resp {
                    eprintln!("zalo-gpui my-documents cloud sync failed: {err}");
                }
            }
            break;
        };
        let root_owned = val
            .get("data")
            .and_then(|d| match d {
                Value::Object(_) => Some(d.clone()),
                Value::String(s) => serde_json::from_str::<Value>(s).ok(),
                _ => None,
            })
            .unwrap_or(val);
        let root = &root_owned;
        let items = root
            .get("groupMsgs")
            .or_else(|| root.get("msgs"))
            .and_then(Value::as_array);
        let Some(items) = items else {
            break;
        };
        let mut min_gid: Option<i64> = None;
        let mut added_this_page = 0usize;
        let mut unseen_locally_this_page = 0usize;
        for raw in items {
            if let Some(mut inc) = parse_item(raw, false, send2me_id) {
                inc.thread_id = send2me_id.to_owned();
                inc.is_group = false;
                if let Some(gid) = inc.global_msg_id {
                    min_gid = Some(min_gid.map_or(gid, |m| m.min(gid)));
                    if !seen_gids.insert(gid) {
                        continue;
                    }
                    if !existing_gids.contains(&gid) {
                        unseen_locally_this_page += 1;
                    }
                } else {
                    unseen_locally_this_page += 1;
                }
                all.push(inc);
                added_this_page += 1;
            }
        }
        let has_more = json_flag(root, "hasMore");
        let resp_is_old = json_flag(root, "isOld");
        let next_cursor = json_str(root, "lastMsgId")
            .filter(|s| !s.is_empty() && s != "0")
            .or_else(|| min_gid.map(|g| g.to_string()));
        if !has_more {
            break;
        }
        if !full_backfill && !existing_gids.is_empty() && unseen_locally_this_page == 0 {
            break;
        }
        let switching_to_old = !use_old && resp_is_old;
        if resp_is_old {
            use_old = true;
        }
        if added_this_page == 0 && next_cursor == cursor && !switching_to_old {
            break;
        }
        if next_cursor.is_some() {
            cursor = next_cursor;
        } else {
            break;
        }
    }

    if all.is_empty() {
        return all;
    }
    all.sort_by_key(|m| (m.at_ms, m.global_msg_id.unwrap_or(0)));
    let latest_at = all.last().map(|m| m.at_ms).unwrap_or(0);
    let sp = store_path.to_path_buf();
    let sid = send2me_id.to_owned();
    let persisted = tokio::task::spawn_blocking(move || {
        if let Ok(mut store) = LocalStore::open(&sp) {
            let _ = store.upsert_threads_batch(&[ThreadRecord {
                thread_id: sid.clone(),
                kind: ThreadKind::User,
                title: "My Documents".to_owned(),
                peer_user_id: Some(sid),
                unread: 0,
                updated_at: latest_at,
            }]);
        }
        store_all(&sp, &mut all);
        all
    })
    .await
    .unwrap_or_default();
    eprintln!("zalo-gpui my-documents cloud synced {} msgs", persisted.len());
    persisted
}

pub(crate) fn preload_messages(root: &Value, self_uin: &str) -> Vec<Incoming> {
    let mut synced = Vec::new();
    for (key, group) in [("msgs", false), ("groupMsgs", true)] {
        if let Some(items) = root.get(key).and_then(Value::as_array) {
            synced.extend(items.iter().filter_map(|v| parse_item(v, group, self_uin)));
        }
    }
    if synced.is_empty() {
        if let Some(items) = root.get("allmsgs").and_then(Value::as_array) {
            synced.extend(
                items
                    .iter()
                    .filter_map(|v| parse_item(v, json_flag(v, "isGroup"), self_uin)),
            );
        }
    }
    synced
}

fn json_flag(value: &Value, key: &str) -> bool {
    match value.get(key) {
        Some(Value::Bool(on)) => *on,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

pub fn parse_item(value: &Value, is_group: bool, self_uin: &str) -> Option<Incoming> {
    let uid_from = json_str(value, "uidFrom").unwrap_or_default();
    let id_to = json_str(value, "idTo").unwrap_or_default();
    let explicit_group_id = json_str(value, "groupId")
        .or_else(|| json_str(value, "grid"))
        .filter(|s| !s.is_empty() && s != "0");
    let is_group = is_group || json_flag(value, "isGroup") || explicit_group_id.is_some();
    let from_me = uid_from == "0" || (!self_uin.is_empty() && uid_from == self_uin);
    let thread_id = if is_group {
        let raw = explicit_group_id.as_deref().or_else(|| {
            (!id_to.is_empty() && id_to != "0" && (self_uin.is_empty() || id_to != self_uin))
                .then_some(id_to.as_str())
        })?;
        let clean = raw
            .strip_prefix("g_")
            .or_else(|| {
                raw.strip_prefix('g')
                    .filter(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
            })
            .unwrap_or(raw);
        if clean.is_empty() {
            return None;
        }
        format!("g_{clean}")
    } else if from_me {
        if id_to.is_empty() || id_to == "0" {
            self_uin.to_owned()
        } else {
            id_to
        }
    } else {
        uid_from.clone()
    };
    if thread_id.is_empty() {
        return None;
    }
    let msg_type = json_str(value, "msgType").unwrap_or_default();
    let raw_content = value.get("content")?;
    let content = as_object(raw_content);
    let params = content.get("params").map(as_object).unwrap_or(Value::Null);
    let field = |key: &str| {
        content
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "sendBubbleMessage")
            .map(str::to_owned)
    };
    let https = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .filter(|u| crate::media::is_http_url(u))
            .map(str::to_owned)
    };
    let mut m = Incoming {
        thread_id,
        is_group,
        from_me,
        sender_uid: if from_me { String::new() } else { uid_from },
        sender_name: json_str(value, "dName").unwrap_or_default(),
        at_ms: value
            .get("ts")
            .and_then(json_i64)
            .map(|t| if t < 10_000_000_000 { t * 1000 } else { t })
            .unwrap_or_else(crate::data::now_ms),
        cli_msg_id: value.get("cliMsgId").and_then(json_i64),
        global_msg_id: value.get("msgId").and_then(json_i64),
        seen_record: (!from_me)
            .then(|| zalo_client::seen_record(value, is_group))
            .flatten(),
        ..Incoming::default()
    };
    let action = field("action").unwrap_or_default();
    match msg_type.as_str() {
        "chat.undo" => return None,
        "chat.photo" => {
            m.kind = MsgKind::Photo;
            let ori = https(params.get("oriUrl")).unwrap_or_default();
            let hd = https(params.get("hd")).unwrap_or_default();
            let href = https(content.get("href")).unwrap_or_default();
            let thumb = https(content.get("thumb"))
                .or_else(|| https(params.get("thumb")))
                .unwrap_or_default();
            m.media_url = crate::media::pick_full_photo_url(&ori, &hd, &href, &thumb);
            m.thumb_url = thumb;
            m.width = params.get("width").and_then(json_i64).unwrap_or(0).max(0) as u32;
            m.height = params.get("height").and_then(json_i64).unwrap_or(0).max(0) as u32;
            m.text = field("title")
                .filter(|t| !crate::media::is_local_media_uri(t))
                .unwrap_or_else(|| "[Hình ảnh]".into());
            m.group_layout_id = photo_group_layout(&params);
        }
        "chat.gif" => {
            // The still `thumb` is often a JPEG. The moving picture is `href`.
            m.kind = MsgKind::Gif;
            m.media_url = https(content.get("href"))
                .or_else(|| https(params.get("hd")))
                .or_else(|| https(content.get("thumb")))
                .unwrap_or_default();
            m.width = params.get("width").and_then(json_i64).unwrap_or(0).max(0) as u32;
            m.height = params.get("height").and_then(json_i64).unwrap_or(0).max(0) as u32;
            m.text = "[GIF]".into();
        }
        "chat.sticker" => {
            m.kind = MsgKind::Sticker;
            m.text = "[Sticker]".into();
            // PC plays `sticker/webpc` (animated WebP) for content `id`.
            if let Some(sticker) = zalo_client::StickerRef::from_value(&content)
                .or_else(|| zalo_client::StickerRef::from_value(&params))
            {
                m.media_url = sticker.image_url();
                m.width = 130;
                m.height = 130;
            }
        }
        "group.poll" => {
            let body = crate::media::poll_from_message(&content);
            let question = if body.question.is_empty() {
                "Bình chọn".to_owned()
            } else {
                body.question.clone()
            };
            m.kind = MsgKind::Poll;
            m.text = format!("[Bình chọn] {question}");
            m.file_name = question;
            m.poll_id = body.id;
            m.poll_question = m.file_name.clone();
            m.poll_action = body.action;
            m.poll_closed = body.closed;
            m.poll_options = body.options;
        }
        "chat.voice" => {
            m.kind = MsgKind::Voice;
            m.text = "[Tin nhắn thoại]".into();
            m.href = https(content.get("href")).unwrap_or_default();
            m.duration_ms = params
                .get("duration")
                .and_then(json_i64)
                .unwrap_or(0)
                .max(0) as u64;
        }
        "chat.video.msg" => {
            m.kind = MsgKind::Video;
            m.text = "[Video]".into();
            m.href = https(content.get("href")).unwrap_or_default();
            m.media_url = https(content.get("thumb")).unwrap_or_default();
            m.duration_ms = params
                .get("duration")
                .and_then(json_i64)
                .unwrap_or(0)
                .max(0) as u64;
            let dim = |a: &str, b: &str| {
                params
                    .get(a)
                    .or_else(|| params.get(b))
                    .and_then(json_i64)
                    .unwrap_or(0)
                    .max(0) as u32
            };
            m.width = dim("video_width", "width");
            m.height = dim("video_height", "height");
        }
        "share.file" => {
            m.kind = MsgKind::File;
            m.file_name = json_str(&content, "title").unwrap_or_else(|| "file".into());
            m.file_size = content
                .get("fileSize")
                .or_else(|| params.get("fileSize"))
                .and_then(json_i64)
                .unwrap_or(0)
                .max(0) as u64;
            m.text = format!("[File] {}", m.file_name);
        }
        "chat.location.new" => {
            m.kind = MsgKind::Location;
            let title = field("title").unwrap_or_default();
            let desc = field("description")
                .or_else(|| field("desc"))
                .unwrap_or_default();
            let lat = params.get("latitude").and_then(json_f64).unwrap_or(0.0);
            let lng = params
                .get("longitude")
                .or_else(|| params.get("lo"))
                .and_then(json_f64)
                .unwrap_or(0.0);
            m.file_name = title.clone();
            m.href = crate::media::maps_href(&field("href").unwrap_or_default(), lat, lng);
            m.media_url = https(content.get("thumb")).unwrap_or_default();
            m.text = if !desc.is_empty() {
                format!("[Vị trí] {desc}")
            } else if !title.is_empty() {
                format!("[Vị trí] {title}")
            } else {
                "[Vị trí]".into()
            };
        }
        "chat.doodle" => {
            m.kind = MsgKind::Doodle;
            m.media_url = https(content.get("href"))
                .or_else(|| https(content.get("thumb")))
                .unwrap_or_default();
            m.width = params.get("width").and_then(json_i64).unwrap_or(0).max(0) as u32;
            m.height = params.get("height").and_then(json_i64).unwrap_or(0).max(0) as u32;
            m.text = "[Vẽ hình]".into();
        }
        "chat.ecard" => {
            m.kind = MsgKind::Ecard;
            let title = field("title").unwrap_or_else(|| "Thiệp".into());
            let desc = field("description").unwrap_or_default();
            m.file_name = title.clone();
            let href = field("href").unwrap_or_default();
            if href.starts_with("http://") || href.starts_with("https://") {
                m.href = href;
            }
            m.media_url = https(content.get("thumb")).unwrap_or_default();
            m.text = if desc.is_empty() {
                format!("[Thiệp] {title}")
            } else {
                format!("[Thiệp] {desc}")
            };
        }
        "chat.delete.everyone" => {
            m.kind = MsgKind::Text;
            m.text = "Tin nhắn đã bị xóa".into();
        }
        "chat.vanish" => {
            m.kind = MsgKind::Text;
            m.text = "[Tin nhắn đã tự xóa]".into();
        }
        "chat.todo" => {
            let title = field("title")
                .or_else(|| field("description"))
                .unwrap_or_else(|| "Việc cần làm".into());
            m.kind = MsgKind::Text;
            m.text = format!("[Việc cần làm] {title}");
        }
        "chat.recommended" | "chat.link" => match action.as_str() {
            "recommened.misscall" => {
                m.kind = MsgKind::MissedCall;
                m.text = "[Cuộc gọi nhỡ]".into();
            }
            "recommened.calltime" => {
                m.kind = MsgKind::Call;
                m.text = "[Cuộc gọi]".into();
            }
            "recommened.user" => {
                m.kind = MsgKind::Card;
                let param_str = |key: &str| {
                    params
                        .get(key)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                };
                let title = field("title")
                    .or_else(|| param_str("title"))
                    .unwrap_or_default();
                let desc = field("description")
                    .or_else(|| field("desc"))
                    .or_else(|| param_str("description"))
                    .or_else(|| param_str("desc"))
                    .unwrap_or_default();
                m.file_name = title.clone();
                m.media_url = https(content.get("thumb"))
                    .or_else(|| https(params.get("avatar")))
                    .or_else(|| https(params.get("thumb")))
                    .unwrap_or_default();
                m.text = if desc.is_empty() {
                    format!("[Danh thiếp] {title}")
                } else {
                    format!("[Danh thiếp] {title}\n{desc}")
                };
            }
            _ => {
                let href = field("href").unwrap_or_default();
                m.link_title = field("title").unwrap_or_default();
                m.link_desc = field("description").unwrap_or_default();
                m.media_url = https(content.get("thumb")).unwrap_or_default();
                m.text = if href.is_empty() {
                    field("title").unwrap_or_else(|| "[Liên kết]".into())
                } else {
                    href
                };
                m.kind = if crate::media::first_link(&m.text).is_some() {
                    MsgKind::Link
                } else {
                    MsgKind::Text
                };
            }
        },
        _ => {
            let text = match raw_content {
                Value::String(s) => {
                    Some(s.trim().to_owned()).filter(|s| !s.is_empty() && s != "sendBubbleMessage")
                }
                _ => field("msg")
                    .or_else(|| field("title"))
                    .or_else(|| field("description")),
            }?;
            m.kind = if crate::media::first_link(&text).is_some_and(|u| u == text) {
                MsgKind::Link
            } else {
                MsgKind::Text
            };
            m.text = text;
        }
    }
    // Electron copies `specialMsgType` onto `message.urgency` (1 important, 2 urgent).
    if let Some(level) = value.get("specialMsgType").and_then(json_i64) {
        m.urgency = level.clamp(0, 255) as u8;
    }
    fill_quote(&mut m, value, self_uin);
    Some(m)
}

fn fill_quote(m: &mut Incoming, value: &Value, self_uin: &str) {
    let Some(q) = value.get("quote").map(as_object).filter(|v| v.is_object()) else {
        return;
    };
    let uid = json_str(&q, "ownerId")
        .or_else(|| json_str(&q, "fromUid"))
        .unwrap_or_default();
    let from_me = uid == "0" || (!self_uin.is_empty() && uid == self_uin);
    let from_d = json_str(&q, "fromD").unwrap_or_default();
    m.quote_sender = if from_me {
        "Bạn".to_owned()
    } else if !from_d.is_empty() {
        from_d
    } else {
        uid
    };
    m.quote_global_id = q
        .get("globalMsgId")
        .or_else(|| q.get("msgId"))
        .and_then(json_i64);
    m.quote_cli_id = q.get("cliMsgId").and_then(json_i64);
    let msg_text = json_str(&q, "msg").unwrap_or_default();
    if !msg_text.is_empty() {
        m.quote_text = msg_text;
        return;
    }
    let attach = q.get("attach").map(as_object).unwrap_or(Value::Null);
    if let Some(title) = json_str(&attach, "title").filter(|s| !s.is_empty()) {
        m.quote_text = title;
        return;
    }
    let cli_type = json_str(&q, "cliMsgType").unwrap_or_default();
    m.quote_text = match cli_type.as_str() {
        "chat.photo" => "[Hình ảnh]".into(),
        "chat.sticker" => "[Sticker]".into(),
        "chat.gif" => "[GIF]".into(),
        "group.poll" => "[Bình chọn]".into(),
        "chat.voice" => "[Tin nhắn thoại]".into(),
        "chat.video.msg" => "[Video]".into(),
        "share.file" => "[File]".into(),
        "chat.location.new" => "[Vị trí]".into(),
        "chat.doodle" => "[Vẽ hình]".into(),
        "chat.ecard" => "[Thiệp]".into(),
        "chat.todo" => "[Việc cần làm]".into(),
        _ if attach.get("thumb").is_some() || attach.get("thumbUrl").is_some() => {
            "[Hình ảnh]".into()
        }
        _ => "[Tin nhắn]".into(),
    };
}

/// Rewrite a row that was stored as plain text before this client knew the kind,
/// or upgrade legacy cropped-thumbnail photo attachments to full-resolution URLs.
fn refresh_richer(store: &LocalStore, m: &Incoming) {
    let Some(gid) = m.global_msg_id else {
        return;
    };
    let marker = m.text == "Tin nhắn đã bị xóa"
        || m.text == "[Tin nhắn đã tự xóa]"
        || m.text.starts_with("[Việc cần làm]");
    let photo_upgrade = m.kind == MsgKind::Photo && !m.media_url.is_empty();
    let rich = matches!(
        m.kind,
        MsgKind::Location | MsgKind::Doodle | MsgKind::Ecard | MsgKind::Card
    );
    if !rich && !marker && !photo_upgrade {
        return;
    }
    let Ok(ids) = store.replace_body_by_global_id(&m.thread_id, gid, &m.text) else {
        return;
    };
    if !rich && !photo_upgrade {
        return;
    }
    let mut data = match m.kind {
        MsgKind::Photo => {
            let thumb = if m.thumb_url.is_empty() {
                m.media_url.as_str()
            } else {
                m.thumb_url.as_str()
            };
            let mut obj = serde_json::json!({
                "kind": "photo",
                "thumb": thumb,
                "hd": m.media_url,
                "hd_checked": true,
                "width": m.width,
                "height": m.height,
                "local": null
            });
            if m.group_layout_id != 0 {
                obj["group_layout_id"] = serde_json::json!(m.group_layout_id);
            }
            obj
        }
        MsgKind::Doodle => serde_json::json!({
            "kind": "doodle", "thumb": m.media_url, "hd": m.media_url,
            "width": m.width, "height": m.height, "local": null
        }),
        MsgKind::Location => serde_json::json!({
            "kind": "location", "title": m.file_name, "href": m.href, "thumb": m.media_url
        }),
        MsgKind::Ecard => serde_json::json!({
            "kind": "ecard", "title": m.file_name, "href": m.href, "thumb": m.media_url
        }),
        MsgKind::Card => serde_json::json!({
            "kind": "card", "title": m.file_name, "thumb": m.media_url, "hd": ""
        }),
        _ => return,
    };
    if !m.quote_text.is_empty() || !m.quote_sender.is_empty() {
        data["quote"] = serde_json::json!({
            "sender": m.quote_sender,
            "text": m.quote_text,
            "gid": m.quote_global_id,
            "cid": m.quote_cli_id,
        });
    }
    for id in ids {
        let _ = store.set_attachment(id, &data.to_string());
    }
}

/// Write new rows (skip ids already stored) and attachments like zalo-gui.
pub(crate) fn store_all(path: &Path, list: &mut [Incoming]) {
    let Ok(store) = LocalStore::open(path) else {
        return;
    };
    for m in list.iter_mut() {
        if let Some(gid) = m.global_msg_id {
            if store.has_global_id(&m.thread_id, gid).unwrap_or(false) {
                // History saved before these kinds were parsed stays plain text
                // until the same server row is seen again.
                refresh_richer(&store, m);
                continue;
            }
        }
        if !m.from_me
            && !m.sender_uid.is_empty()
            && (!m.sender_name.is_empty() || store.user(&m.sender_uid).ok().flatten().is_none())
        {
            let display_name = if m.sender_name.is_empty() {
                m.sender_uid.clone()
            } else {
                m.sender_name.clone()
            };
            let _ = store.upsert_user(&UserRecord {
                user_id: m.sender_uid.clone(),
                display_name,
                zalo_name: String::new(),
                updated_at: m.at_ms,
            });
        }
        if store
            .threads()
            .map(|t| !t.iter().any(|r| r.thread_id == m.thread_id))
            .unwrap_or(false)
        {
            let title = if m.is_group {
                m.thread_id.clone()
            } else if !m.sender_name.is_empty() {
                m.sender_name.clone()
            } else {
                m.thread_id.clone()
            };
            let _ = store.upsert_thread(&ThreadRecord {
                thread_id: m.thread_id.clone(),
                kind: if m.is_group {
                    ThreadKind::Group
                } else {
                    ThreadKind::User
                },
                title,
                peer_user_id: (!m.is_group && !m.from_me && !m.sender_uid.is_empty())
                    .then(|| m.sender_uid.clone()),
                unread: 0,
                updated_at: m.at_ms,
            });
        }
        let record = MessageRecord {
            message_id: 0,
            thread_id: m.thread_id.clone(),
            sender_id: (!m.from_me).then(|| m.sender_uid.clone()),
            cli_msg_id: m.cli_msg_id,
            global_msg_id: m.global_msg_id,
            body: m.text.clone(),
            sent_at: m.at_ms,
        };
        if let Ok(row) = store.insert_message(&record) {
            m.row_id = row;
            let mut attach = if matches!(m.kind, MsgKind::Photo | MsgKind::Gif)
                && !m.media_url.is_empty()
            {
                let kind = if m.kind == MsgKind::Gif {
                    "gif"
                } else {
                    "photo"
                };
                {
                    let thumb = if m.thumb_url.is_empty() {
                        m.media_url.as_str()
                    } else {
                        m.thumb_url.as_str()
                    };
                    let mut obj = serde_json::json!({
                        "kind": kind, "thumb": thumb, "hd": m.media_url,
                        "hd_checked": true,
                        "width": m.width, "height": m.height, "local": null
                    });
                    if m.group_layout_id != 0 {
                        obj["group_layout_id"] = serde_json::json!(m.group_layout_id);
                    }
                    Some(obj)
                }
            } else if m.kind == MsgKind::Sticker && !m.media_url.is_empty() {
                Some(serde_json::json!({
                    "kind": "sticker", "thumb": m.media_url, "hd": m.media_url,
                    "width": m.width, "height": m.height, "local": null
                }))
            } else if m.kind == MsgKind::Poll && m.poll_id != 0 {
                Some(serde_json::json!({
                    "kind": "poll",
                    "poll_id": m.poll_id,
                    "question": m.poll_question,
                    "action": m.poll_action,
                    "closed": m.poll_closed,
                    "options": m.poll_options.iter().map(|o| serde_json::json!({
                        "id": o.id, "content": o.content, "votes": o.votes, "voted": o.voted
                    })).collect::<Vec<_>>()
                }))
            } else if m.kind == MsgKind::Doodle && !m.media_url.is_empty() {
                Some(serde_json::json!({
                    "kind": "doodle", "thumb": m.media_url, "hd": m.media_url,
                    "width": m.width, "height": m.height, "local": null
                }))
            } else if m.kind == MsgKind::Location {
                Some(serde_json::json!({
                    "kind": "location", "title": m.file_name, "href": m.href, "thumb": m.media_url
                }))
            } else if m.kind == MsgKind::Ecard {
                Some(serde_json::json!({
                    "kind": "ecard", "title": m.file_name, "href": m.href, "thumb": m.media_url
                }))
            } else if m.kind == MsgKind::Card {
                Some(serde_json::json!({
                    "kind": "card", "title": m.file_name, "thumb": m.media_url, "hd": ""
                }))
            } else if matches!(m.kind, MsgKind::Voice | MsgKind::Video) && !m.href.is_empty() {
                // Voice / video: zalo-gui ignores unknown kinds, GPUI reads them back.
                let kind = if m.kind == MsgKind::Voice {
                    "voice"
                } else {
                    "video"
                };
                Some(serde_json::json!({
                    "kind": kind, "href": m.href, "thumb": m.media_url, "hd": "",
                    "width": m.width, "height": m.height, "size": m.duration_ms, "local": null
                }))
            } else {
                None
            };
            if !m.quote_text.is_empty() || !m.quote_sender.is_empty() {
                let mut obj = attach.unwrap_or_else(|| serde_json::json!({ "kind": "text" }));
                obj["quote"] = serde_json::json!({
                    "sender": m.quote_sender,
                    "text": m.quote_text,
                    "gid": m.quote_global_id,
                    "cid": m.quote_cli_id,
                });
                attach = Some(obj);
            }
            if let Some(data) = attach {
                let _ = store.set_attachment(row, &data.to_string());
            }
        }
    }
}

/// Parse reaction items (`reacts` and `reactGroups`) from WS `610/611/612/613`.
#[must_use]
pub fn reactions_from_event(cmd: u16, event: &Value, self_uin: &str) -> Vec<ReactionUpdate> {
    let root = event.get("data").filter(|v| v.is_object()).unwrap_or(event);
    let mut out = Vec::new();
    for (key, is_group) in [("reacts", false), ("reactGroups", true)] {
        let Some(items) = root
            .get(key)
            .or_else(|| event.get(key))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for item in items {
            collect_reaction_item(cmd, item, is_group, self_uin, &mut out);
        }
    }
    out
}

fn collect_reaction_item(
    cmd: u16,
    item: &Value,
    is_group: bool,
    self_uin: &str,
    out: &mut Vec<ReactionUpdate>,
) {
    let uid_from = json_str(item, "uidFrom").unwrap_or_default();
    let id_to = json_str(item, "idTo").unwrap_or_default();
    let from_me = uid_from == "0" || (!self_uin.is_empty() && uid_from == self_uin);
    let sender_uid = if from_me {
        if self_uin.is_empty() {
            "me".to_owned()
        } else {
            self_uin.to_owned()
        }
    } else {
        uid_from.clone()
    };
    let thread_id = if is_group {
        let raw = if !id_to.is_empty() && id_to != "0" {
            &id_to
        } else {
            &uid_from
        };
        let clean = raw.trim_start_matches("g_").trim_start_matches('g');
        if clean.is_empty() {
            String::new()
        } else {
            format!("g_{clean}")
        }
    } else if !from_me && (id_to.is_empty() || id_to == "0" || id_to == self_uin) {
        uid_from.clone()
    } else if id_to.is_empty() || id_to == "0" {
        if from_me {
            self_uin.to_owned()
        } else {
            uid_from.clone()
        }
    } else {
        id_to.clone()
    };
    if thread_id.is_empty() {
        return;
    }
    let Some(content) = item.get("content").map(as_object) else {
        return;
    };
    let r_type = content.get("rType").and_then(json_i64).unwrap_or(0);
    let r_icon = json_str(&content, "rIcon").unwrap_or_default();
    let cleared = cmd == zalo_client::listener::CMD_CLEAR_REACT || r_type < 0;
    let Some(targets) = content.get("rMsg").and_then(Value::as_array) else {
        return;
    };
    for rm in targets {
        let target_global_id = rm.get("gMsgID").and_then(json_i64);
        let target_cli_id = rm.get("cMsgID").and_then(json_i64);
        if target_global_id.is_none() && target_cli_id.is_none() {
            continue;
        }
        out.push(ReactionUpdate {
            thread_id: thread_id.clone(),
            is_group,
            target_global_id,
            target_cli_id,
            from_me,
            sender_uid: sender_uid.clone(),
            r_icon: r_icon.clone(),
            r_type,
            cleared,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_voice_video_and_link_card() {
        let voice = serde_json::json!({"uidFrom": "5", "msgType": "chat.voice",
            "content": {"href": "https://voice.example/a.m4a", "params": "{\"duration\":3240}"}});
        let v = parse_item(&voice, false, "9").unwrap();
        assert_eq!(
            (v.kind, v.href.as_str(), v.duration_ms),
            (MsgKind::Voice, "https://voice.example/a.m4a", 3240)
        );
        let video = serde_json::json!({"uidFrom": "5", "msgType": "chat.video.msg",
            "content": {"href": "https://video.example/a.mp4", "thumb": "https://video.example/a.jpg",
                "params": "{\"duration\":65000,\"video_width\":720,\"video_height\":1280}"}});
        let v = parse_item(&video, false, "9").unwrap();
        assert_eq!(v.kind, MsgKind::Video);
        assert_eq!((v.width, v.height, v.duration_ms), (720, 1280, 65_000));
        assert_eq!(v.media_url, "https://video.example/a.jpg");
        let gif = serde_json::json!({"uidFrom": "5", "msgType": "chat.gif",
            "content": {"thumb": "https://g.example/a.jpg", "href": "https://g.example/a.gif",
                "params": "{\"width\":120,\"height\":80}"}});
        let g = parse_item(&gif, false, "9").unwrap();
        assert_eq!(g.kind, MsgKind::Gif);
        assert_eq!(g.media_url, "https://g.example/a.gif");
        let sticker = serde_json::json!({"uidFrom": "5", "msgType": "chat.sticker",
            "content": {"id": 25826, "catId": 1, "type": 7}});
        let s = parse_item(&sticker, false, "9").unwrap();
        assert_eq!(s.kind, MsgKind::Sticker);
        assert!(s.media_url.contains("eid=25826"));
        assert!(s.media_url.contains("webpc"));
        let poll = serde_json::json!({"uidFrom": "5", "idTo": "2641", "msgType": "group.poll",
            "content": {"action": "create", "params": "{\"pollId\":42,\"question\":\"Ăn gì?\"}"}});
        let p = parse_item(&poll, true, "9").unwrap();
        assert_eq!(p.kind, MsgKind::Poll);
        assert_eq!(p.poll_id, 42);
        assert_eq!(p.file_name, "Ăn gì?");
        assert_eq!(p.text, "[Bình chọn] Ăn gì?");
        assert_eq!(p.poll_action, "create");
        let link = serde_json::json!({"uidFrom": "5", "msgType": "chat.recommended",
            "content": {"action": "", "href": "https://dev.to/x", "title": "T", "description": "D", "thumb": "https://t/x.jpg"}});
        let l = parse_item(&link, false, "9").unwrap();
        assert_eq!(
            (l.kind, l.link_title.as_str(), l.link_desc.as_str()),
            (MsgKind::Link, "T", "D")
        );
        let person = serde_json::json!({"uidFrom": "5", "msgType": "chat.recommended",
            "content": {"action": "recommened.user", "title": "Lan", "description": "Bạn",
                "thumb": "https://ava.example/a.jpg"}});
        let c = parse_item(&person, false, "9").unwrap();
        assert_eq!(c.kind, MsgKind::Card);
        assert_eq!(c.file_name, "Lan");
        assert_eq!(c.media_url, "https://ava.example/a.jpg");
        assert_eq!(c.text, "[Danh thiếp] Lan\nBạn");
    }

    #[test]
    fn zero_preload_map_marks_known_threads_as_empty() {
        let rows = serde_json::json!([
            {"idTo": "5", "isGroup": 0, "lastMsgId": "9"},
            {"idTo": "g_12", "isGroup": true, "lastMsgId": "3"},
            {"idTo": "0", "isGroup": 0}
        ]);
        let map = zero_preload_map(Some(&rows)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&map).unwrap();
        assert_eq!(value["5_0"], "0");
        assert_eq!(value["12_1"], "0");
        assert!(value.get("0_0").is_none());
    }

    #[test]
    fn preload_reads_allmsgs_when_split_arrays_are_absent() {
        let page = serde_json::json!({
            "allmsgs": [
                {"uidFrom": "5", "idTo": "0", "isGroup": false, "msgType": "webchat", "content": "Chào", "msgId": "77"},
                {"uidFrom": "0", "idTo": "2641", "isGroup": 1, "msgType": "webchat", "content": "Nhóm", "msgId": "78"}
            ]
        });
        let rows = preload_messages(&page, "999");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].thread_id, "5");
        assert!(!rows[0].is_group);
        assert_eq!(rows[1].thread_id, "g_2641");
        assert!(rows[1].is_group);
    }

    #[test]
    fn parses_text_photo_file_and_skips_undo() {
        let me = "999";
        let text = serde_json::json!({"uidFrom": "5", "idTo": "0", "msgType": "webchat", "content": "Chào", "ts": "1794400000000", "msgId": "77", "cliMsgId": "66", "dName": "Lan"});
        let t = parse_item(&text, false, me).unwrap();
        assert_eq!(
            (t.thread_id.as_str(), t.text.as_str(), t.from_me),
            ("5", "Chào", false)
        );
        assert_eq!(t.global_msg_id, Some(77));
        let photo = serde_json::json!({"uidFrom": "0", "idTo": "2641", "msgType": "chat.photo",
            "content": {"thumb": "https://t.example/a.jpg", "params": "{\"width\":640,\"height\":360}"}, "ts": 1794400000});
        let p = parse_item(&photo, true, me).unwrap();
        assert_eq!(p.thread_id, "g_2641");
        assert!(p.from_me);
        assert_eq!(
            (p.kind, p.width, p.at_ms),
            (MsgKind::Photo, 640, 1_794_400_000_000)
        );
        let file = serde_json::json!({"uidFrom": "5", "msgType": "share.file", "content": {"title": "a.pdf", "fileSize": "1024"}});
        let f = parse_item(&file, false, me).unwrap();
        assert_eq!(
            (f.kind, f.file_name.as_str(), f.file_size),
            (MsgKind::File, "a.pdf", 1024)
        );
        let undo = serde_json::json!({"uidFrom": "5", "msgType": "chat.undo", "content": {}});
        assert!(parse_item(&undo, false, me).is_none());
        let place = serde_json::json!({"uidFrom": "5", "msgType": "chat.location.new",
            "content": {"title": "Hồ Gươm", "description": "Hoàn Kiếm",
                "params": "{\"latitude\":21.0285,\"longitude\":105.8542}"}});
        let loc = parse_item(&place, false, me).unwrap();
        assert_eq!(loc.kind, MsgKind::Location);
        assert_eq!(loc.file_name, "Hồ Gươm");
        assert_eq!(loc.text, "[Vị trí] Hoàn Kiếm");
        assert!(loc.href.contains("21.0285"));
        let doodle = serde_json::json!({"uidFrom": "5", "msgType": "chat.doodle",
            "content": {"href": "https://d.example/a.png", "params": "{\"width\":200,\"height\":80}"}});
        let d = parse_item(&doodle, false, me).unwrap();
        assert_eq!(
            (d.kind, d.media_url.as_str(), d.width),
            (MsgKind::Doodle, "https://d.example/a.png", 200)
        );
        let card = serde_json::json!({"uidFrom": "5", "msgType": "chat.ecard",
            "content": {"title": "Kết bạn", "description": "Hai bạn đã trở thành bạn bè"}});
        let e = parse_item(&card, false, me).unwrap();
        assert_eq!(e.kind, MsgKind::Ecard);
        assert_eq!(e.file_name, "Kết bạn");
        assert_eq!(e.text, "[Thiệp] Hai bạn đã trở thành bạn bè");
        let gone =
            serde_json::json!({"uidFrom": "5", "msgType": "chat.delete.everyone", "content": {}});
        assert_eq!(
            parse_item(&gone, false, me).unwrap().text,
            "Tin nhắn đã bị xóa"
        );
        let task = serde_json::json!({"uidFrom": "5", "msgType": "chat.todo", "content": {"title": "Gọi lại"}});
        assert_eq!(
            parse_item(&task, false, me).unwrap().text,
            "[Việc cần làm] Gọi lại"
        );
        let gone_ttl = serde_json::json!({"uidFrom": "5", "msgType": "chat.vanish", "content": {}});
        assert_eq!(
            parse_item(&gone_ttl, false, me).unwrap().text,
            "[Tin nhắn đã tự xóa]"
        );
        let urgent = serde_json::json!({"uidFrom": "5", "msgType": "webchat", "content": "Khẩn", "specialMsgType": 2});
        assert_eq!(parse_item(&urgent, false, me).unwrap().urgency, 2);
        assert_eq!(parse_item(&doodle, false, me).unwrap().text, "[Vẽ hình]");
        let page = serde_json::json!({"message": {"msgs": [{"uidFrom": "5", "msgType": "webchat", "content": "Khẩn"}], "groupMsgs": []}});
        let root = important_root(&page);
        assert_eq!(root["msgs"][0]["content"], "Khẩn");
        let wrapped = serde_json::json!({"data": {"message": "{\"msgs\":[{\"content\":\"A\"}]}"}});
        assert_eq!(important_root(&wrapped)["msgs"][0]["content"], "A");
    }

    #[test]
    fn parses_reactions_direct_group_and_clear() {
        let body = serde_json::json!({"data": {
            "reacts": [{
                "uidFrom": "5",
                "idTo": "0",
                "content": "{\"rIcon\":\"/-heart\",\"rType\":5,\"rMsg\":[{\"gMsgID\":\"77\",\"cMsgID\":66}]}"
            }],
            "reactGroups": [{
                "uidFrom": "0",
                "idTo": "2641",
                "content": {"rIcon": "/-strong", "rType": -1, "rMsg": [{"gMsgID": 88, "cMsgID": 12}]}
            }]
        }});
        let list = reactions_from_event(zalo_client::listener::CMD_PUSH_REACT, &body, "999");
        assert_eq!(list.len(), 2);
        assert_eq!(
            (
                &list[0].thread_id,
                list[0].is_group,
                list[0].target_global_id,
                list[0].r_icon.as_str(),
                list[0].cleared
            ),
            (&"5".to_owned(), false, Some(77), "/-heart", false)
        );
        assert_eq!(
            (
                &list[1].thread_id,
                list[1].is_group,
                list[1].from_me,
                list[1].target_global_id,
                list[1].cleared
            ),
            (&"g_2641".to_owned(), true, true, Some(88), true)
        );
        let cleared = reactions_from_event(
            zalo_client::listener::CMD_CLEAR_REACT,
            &serde_json::json!({"data": {
                "reacts": [{"uidFrom": "5", "idTo": "999", "content": {"rIcon": "/-heart", "rType": 5, "rMsg": [{"gMsgID": 77}]}}]
            }}),
            "999",
        );
        assert_eq!(cleared.len(), 1);
        assert!(cleared[0].cleared);
        assert_eq!(cleared[0].thread_id, "5");
    }

    #[test]
    fn parses_quoted_reply_text_and_media_fallback() {
        let quoted_text = serde_json::json!({
            "uidFrom": "5",
            "idTo": "0",
            "msgType": "webchat",
            "content": "Đồng ý nhé",
            "dName": "Lan",
            "quote": {
                "ownerId": "999",
                "fromD": "Tôi",
                "globalMsgId": "111",
                "cliMsgId": "222",
                "msg": "Họp 9h sáng mai"
            }
        });
        let q = parse_item(&quoted_text, false, "999").unwrap();
        assert_eq!(q.quote_sender, "Bạn");
        assert_eq!(q.quote_text, "Họp 9h sáng mai");
        assert_eq!((q.quote_global_id, q.quote_cli_id), (Some(111), Some(222)));

        let quoted_photo = serde_json::json!({
            "uidFrom": "0",
            "idTo": "2641",
            "msgType": "webchat",
            "content": "Ảnh đẹp quá",
            "quote": {
                "fromUid": "5",
                "fromD": "Lan",
                "msgId": 333,
                "cliMsgId": 444,
                "cliMsgType": "chat.photo",
                "attach": "{\"thumb\":\"https://t.example/p.jpg\"}"
            }
        });
        let p = parse_item(&quoted_photo, true, "999").unwrap();
        assert_eq!(p.quote_sender, "Lan");
        assert_eq!(p.quote_text, "[Hình ảnh]");
        assert_eq!((p.quote_global_id, p.quote_cli_id), (Some(333), Some(444)));
    }

    #[test]
    fn mixed_group_item_routes_by_group_id_and_never_creates_fake_sender_group() {
        let mixed_group = serde_json::json!({
            "uidFrom": "555",
            "idTo": "7777",
            "isGroup": 1,
            "msgType": "webchat",
            "content": "hello group"
        });
        let m = parse_item(&mixed_group, false, "999").expect("group msg");
        assert!(m.is_group);
        assert_eq!(m.thread_id, "g_7777");

        let bad_group = serde_json::json!({
            "uidFrom": "555",
            "idTo": "0",
            "msgType": "webchat",
            "content": "orphan"
        });
        assert!(parse_item(&bad_group, true, "999").is_none());
    }
}
