//! "Sync Messages from Phone" (sync v2), wired to the realtime socket.
//!
//! Server keeps only recent history; older history comes from the phone via
//! `cmd 590` (request) -> `cmd 601 transfer_sync2` (confirm + batch URLs) ->
//! encrypted batch download + `zalo_sync2::ZprotoWasm` decode -> `cmd 591` ack.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::Value;
use zalo_client::{Device, Frame, StoredSession, ZaloApi};
use zalo_sync2::request::{
    dispose_body, parse_control, wake_up_body, BatchType, Database, MobileStatus, Query,
    CMD_ACK_DELETE_SESSION, CMD_REQUEST_SYNC, CMD_REQUEST_WAKE_UP, NO_UPPER_BOUND,
};
use zalo_sync2::session::{Decoded, SyncSession, Update};
use zalo_sync2::{keys::KeyPair, wasm::ZprotoWasm, Sync2Error};

use crate::realtime::{parse_item, store_all, sync_outgoing, Incoming};
use crate::shell::ShellView;

/// Wasm file name prefix in Zalo PC's `pc-dist/libs/`.
const WASM_PREFIX: &str = "libzproto_wasm_bg";
/// How long the phone has to answer before a wake-up push is sent.
const WAKE_AFTER: Duration = Duration::from_secs(20);
/// How long to wait for the user to confirm on the phone.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(180);
/// Longest gap between two batches once the phone has confirmed.
const BATCH_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest batch file accepted (64 MiB).
const MAX_BATCH_BYTES: usize = 64 * 1024 * 1024;

static RUNNING: AtomicBool = AtomicBool::new(false);
static NEXT_REQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Relaxed);
    }
}

/// True while a background phone-sync worker is active.
pub(crate) fn is_running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

/// What the phone-sync worker reports to the settings UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Loading WASM and account id map.
    Preparing,
    /// `cmd 590` sent; waiting for phone wake-up.
    Requested,
    /// The phone is showing the confirm dialog.
    WaitingConfirm,
    /// User accepted on phone; waiting for first batch.
    Confirmed,
    /// Batches decoded so far and messages stored.
    Receiving {
        /// Decoded batch count.
        batches: u32,
        /// Converted & stored message count.
        messages: usize,
    },
    /// Completed successfully.
    Done {
        /// Total synced messages.
        messages: usize,
    },
    /// Failed with a localized explanation.
    Failed(String),
}

/// Socket traffic forwarded to the active worker (`cmd 590` reply / `cmd 601` control).
pub(crate) enum Signal {
    Reply(Value),
    Control(Value),
}

/// Updates streamed from the worker thread to [`ShellView`].
enum SyncUpdate {
    Progress(Progress),
    Batch(Vec<Incoming>),
    Status(String),
}

/// Which pinned conversation-list banner to draw. Pure so tests never start a sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncBannerKind {
    /// Logged out, or the user closed the suggestion.
    Hidden,
    /// Offer sync-2. A click is the only thing that starts it.
    Suggest,
    /// A sync the user already started is still running.
    Progress,
    /// The last run finished and the user has not dismissed it.
    /// A completed run clears this immediately so the banner hides itself.
    Success,
}

/// Banner next to the conversation list.
///
/// Progress wins over a previous dismissal so a sync started from Settings
/// stays visible. A finished run dismisses the banner instead of leaving
/// the success row up.
#[must_use]
pub(crate) fn sync_banner_kind(
    has_session: bool,
    dismissed: bool,
    active: bool,
    finished_ok: bool,
) -> SyncBannerKind {
    if !has_session {
        return SyncBannerKind::Hidden;
    }
    if active {
        return SyncBannerKind::Progress;
    }
    if finished_ok && !dismissed {
        return SyncBannerKind::Success;
    }
    if dismissed {
        return SyncBannerKind::Hidden;
    }
    SyncBannerKind::Suggest
}

/// UI state stored on [`ShellView`].
#[derive(Default)]
pub(crate) struct PhoneSyncState {
    pub(crate) to_worker: Option<Sender<Signal>>,
    pub(crate) progress: Option<Progress>,
    /// Last run reached [`Progress::Done`]. Cleared when a new run starts or fails.
    pub(crate) finished_ok: bool,
}

impl PhoneSyncState {
    /// True when the worker is currently running.
    pub(crate) fn active(&self) -> bool {
        self.to_worker.is_some()
    }

    /// Short localized status text for the Settings row.
    pub(crate) fn status_line(&self) -> Option<String> {
        Some(progress_line(self.progress.as_ref()?))
    }
}

/// Human-readable status line for [`Progress`].
#[must_use]
pub(crate) fn progress_line(p: &Progress) -> String {
    match p {
        Progress::Preparing => "Đang chuẩn bị…".into(),
        Progress::Requested => "Đã gửi yêu cầu, đang chờ điện thoại…".into(),
        Progress::WaitingConfirm => "Hãy bấm Đồng ý trên điện thoại.".into(),
        Progress::Confirmed => "Điện thoại đã đồng ý, đang chờ dữ liệu…".into(),
        Progress::Receiving { batches, messages } => {
            format!("Đã nhận {batches} lô, {messages} tin…")
        }
        Progress::Done { messages } => format!("Xong: đã nhận {messages} tin."),
        Progress::Failed(err) => err.clone(),
    }
}

/// Locate `libzproto_wasm_bg.*.wasm` via `$ZALO_ZPROTO_WASM`, XDG data dir,
/// or Zalo PC's decoded bundle directory.
#[must_use]
pub(crate) fn find_wasm() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ZALO_ZPROTO_WASM").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    let mut dirs = Vec::new();
    if !cfg!(test) {
        if let Ok(xdg) = zalo_core::XdgDirs::from_env() {
            dirs.push(xdg.data_home().join("zalo-rust"));
        }
    }
    dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/wasm"));
    dirs.into_iter().find_map(|dir| {
        std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(WASM_PREFIX) && n.ends_with(".wasm"))
            })
    })
}

fn json_str(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn json_i64(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.parse().ok())
}

fn group_ids_from_json(list: &Value) -> Vec<String> {
    let root = list.get("data").filter(|v| v.is_object()).unwrap_or(list);
    root.get("gridVerMap")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Extract `controls` entries from a `cmd 601` push frame.
#[must_use]
pub(crate) fn controls_of(data: &Value) -> Vec<Value> {
    let root = data.get("data").filter(|v| v.is_object()).unwrap_or(data);
    root.get("controls")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|c| {
                    c.get("content")
                        .filter(|v| v.is_object())
                        .unwrap_or(c)
                        .clone()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Bidirectional PC id <-> `globalId` map built from friends, self, and groups.
#[derive(Default)]
pub(crate) struct IdMaps {
    users: HashMap<String, String>,
    groups: HashMap<String, String>,
    pub(crate) me: String,
}

impl IdMaps {
    fn from_values(friends: &Value, me: &Value, groups: &[Value]) -> Self {
        let mut out = Self::default();
        let list = friends.get("data").unwrap_or(friends);
        for friend in list.as_array().into_iter().flatten() {
            if let (Some(uid), Some(gid)) =
                (json_str(friend, "userId"), json_str(friend, "globalId"))
            {
                out.users.insert(gid, uid);
            }
        }
        let me = me.get("data").filter(|v| v.is_object()).unwrap_or(me);
        if let (Some(uid), Some(gid)) = (json_str(me, "userId"), json_str(me, "globalId")) {
            out.users.insert(gid, uid.clone());
            out.me = uid;
        }
        for info in groups {
            let root = info.get("data").filter(|v| v.is_object()).unwrap_or(info);
            let Some(map) = root.get("gridInfoMap").and_then(Value::as_object) else {
                continue;
            };
            for (key, group) in map {
                let id = json_str(group, "groupId").unwrap_or_else(|| key.clone());
                if let Some(gid) = json_str(group, "globalId") {
                    out.groups.insert(gid, id);
                }
            }
        }
        out
    }

    fn pc_id(&self, global: &str, group: bool) -> Option<String> {
        let map = if group { &self.groups } else { &self.users };
        Some(
            map.get(global)
                .cloned()
                .unwrap_or_else(|| global.to_owned()),
        )
    }

    fn queries(&self) -> Vec<Query> {
        let mut out: Vec<Query> = self
            .groups
            .keys()
            .map(|gid| Query::messages(gid, true, 0, NO_UPPER_BOUND))
            .collect();
        out.extend(
            self.users
                .iter()
                .filter(|(_, uid)| **uid != self.me)
                .map(|(gid, _)| Query::messages(gid, false, 0, NO_UPPER_BOUND)),
        );
        out
    }
}

/// Convert decoded protobuf chunks into [`Incoming`] messages (never triggers outbound `seen`).
pub(crate) fn to_synced(
    chunks: &[zalo_sync2::proto::MessageChunk],
    ids: &IdMaps,
    seen: &mut HashSet<i64>,
) -> Vec<Incoming> {
    let map = |global: &str, group: bool| ids.pc_id(global, group);
    let mut out = Vec::new();
    for chunk in chunks {
        for message in zalo_sync2::message::convert_chunk(chunk, &map, &ids.me, seen) {
            let value = message.to_server_json();
            if let Some(mut msg) = parse_item(&value, message.is_group, &ids.me) {
                msg.seen_record = None;
                out.push(msg);
            }
        }
    }
    out
}

/// Add `reqId` matching Zalo PC's `genNextReqId`.
#[must_use]
pub(crate) fn with_req_id(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(map) = body.as_object_mut() {
        let n = NEXT_REQ.fetch_add(1, Ordering::Relaxed);
        map.insert("reqId".into(), Value::String(format!("req_{n}")));
    }
    body
}

fn send(
    out: &tokio::sync::mpsc::UnboundedSender<Frame>,
    cmd: u16,
    body: &Value,
) -> Result<(), String> {
    out.send(Frame::json(cmd, 0, &with_req_id(body)))
        .map_err(|_| "Mất kết nối realtime; hãy thử lại khi đã kết nối.".into())
}

fn sync_err(err: Sync2Error) -> String {
    err.to_string()
}

fn worker(
    dir: &Path,
    ui_tx: futures::channel::mpsc::UnboundedSender<SyncUpdate>,
    out: tokio::sync::mpsc::UnboundedSender<Frame>,
    signals: Receiver<Signal>,
) {
    let _guard = RunningGuard;
    let report = |p: Progress| {
        let _ = ui_tx.unbounded_send(SyncUpdate::Progress(p));
    };
    report(Progress::Preparing);
    let res = run_session(dir, &ui_tx, &out, &signals, &report);
    report(match res {
        Ok(messages) => Progress::Done { messages },
        Err(err) => Progress::Failed(err),
    });
}

fn run_session(
    dir: &Path,
    ui_tx: &futures::channel::mpsc::UnboundedSender<SyncUpdate>,
    out: &tokio::sync::mpsc::UnboundedSender<Frame>,
    signals: &Receiver<Signal>,
    report: &dyn Fn(Progress),
) -> Result<usize, String> {
    let wasm_path = find_wasm().ok_or_else(|| {
        "Không thấy libzproto_wasm_bg.*.wasm của Zalo PC (chép vào ~/.local/share/zalo-rust/)."
            .to_owned()
    })?;
    let mut wasm = ZprotoWasm::from_file(&wasm_path).map_err(sync_err)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let (api, ids, device_name) = rt.block_on(async {
        let device = Device::load_or_create(dir).map_err(|e| e.to_string())?;
        let name = device.computer_name.clone();
        let session = StoredSession::load(dir).map_err(|e| e.to_string())?;
        let mut api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
        api.get_login_info().await.map_err(|e| e.to_string())?;
        let friends = api.get_friends().await.map_err(|e| e.to_string())?;
        let me = api.get_me().await.map_err(|e| e.to_string())?;
        let mut groups = Vec::new();
        let list = api.get_group_list().await.map_err(|e| e.to_string())?;
        let gids = group_ids_from_json(&list);
        for chunk in gids.chunks(50) {
            groups.push(
                api.get_multi_group_info(chunk)
                    .await
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok::<_, String>((api, IdMaps::from_values(&friends, &me, &groups), name))
    })?;
    if ids.me.is_empty() {
        return Err("Không đọc được globalId của tài khoản.".into());
    }
    let queries = ids.queries();
    if queries.is_empty() {
        return Err("Chưa có hội thoại nào để đồng bộ.".into());
    }
    let identity = KeyPair::generate(&mut wasm).map_err(sync_err)?;
    let mut session = SyncSession::new(&mut wasm, identity, Database::Message).map_err(sync_err)?;
    let _ = ui_tx.unbounded_send(SyncUpdate::Status(format!(
        "Đồng bộ {} hội thoại…",
        queries.len()
    )));
    let request = session.request(queries, "", &device_name, 0);
    send(out, CMD_REQUEST_SYNC, &request.body())?;
    report(Progress::Requested);

    let store_path = dir.join("store.db");
    let started = Instant::now();
    let mut woke = false;
    let mut confirmed = false;
    let mut last_batch = Instant::now();
    let mut pending: Vec<(zalo_sync2::request::BatchInfo, Vec<u8>)> = Vec::new();
    let mut seen = HashSet::new();
    let mut batches = 0_u32;
    let mut messages = 0_usize;
    loop {
        if confirmed && session.is_complete() && pending.is_empty() {
            let _ = send(
                out,
                CMD_ACK_DELETE_SESSION,
                &dispose_body(session.sync_id(), 0),
            );
            return Ok(messages);
        }
        let signal = match signals.recv_timeout(Duration::from_secs(1)) {
            Ok(signal) => Some(signal),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return Err("Đã hủy đồng bộ.".into()),
        };
        if !confirmed {
            if !woke && started.elapsed() > WAKE_AFTER && session.last_status().is_none() {
                woke = true;
                send(out, CMD_REQUEST_WAKE_UP, &wake_up_body(session.sync_id()))?;
            }
            if started.elapsed() > CONFIRM_TIMEOUT {
                let _ = send(
                    out,
                    CMD_ACK_DELETE_SESSION,
                    &dispose_body(session.sync_id(), 1),
                );
                return Err(
                    "Điện thoại không trả lời. Hãy mở Zalo trên điện thoại rồi thử lại.".into(),
                );
            }
        } else if last_batch.elapsed() > BATCH_TIMEOUT {
            let _ = send(
                out,
                CMD_ACK_DELETE_SESSION,
                &dispose_body(session.sync_id(), 1),
            );
            return Err(format!("Ngừng nhận dữ liệu sau {batches} lô."));
        }
        let control = match signal {
            None => continue,
            Some(Signal::Reply(body)) => {
                let code = body
                    .get("error_code")
                    .or_else(|| body.get("data").and_then(|d| d.get("error_code")))
                    .and_then(json_i64)
                    .unwrap_or(0);
                if code != 0 {
                    return Err(format!("Máy chủ từ chối yêu cầu đồng bộ (mã {code})."));
                }
                continue;
            }
            Some(Signal::Control(control)) => control,
        };
        let Some(event) = parse_control(&control) else {
            continue;
        };
        match session.on_control(&event) {
            Update::Status(status) => match status {
                MobileStatus::WaitingConfirm => report(Progress::WaitingConfirm),
                MobileStatus::Confirmed | MobileStatus::BypassConfirmed => {
                    confirmed = true;
                    last_batch = Instant::now();
                    report(Progress::Confirmed);
                }
                status if status.is_terminal() => {
                    return Err(match status {
                        MobileStatus::UserReject | MobileStatus::UserCancel => {
                            "Đã từ chối trên điện thoại.".into()
                        }
                        MobileStatus::Busy => "Điện thoại đang đồng bộ với thiết bị khác.".into(),
                        MobileStatus::LowStorage => "Điện thoại sắp hết dung lượng.".into(),
                        _ => "Zalo trên điện thoại đã đăng xuất.".into(),
                    });
                }
                _ => {}
            },
            Update::Download(info) => {
                confirmed = true;
                last_batch = Instant::now();
                let file = rt
                    .block_on(api.fetch_limited(&info.msg_url, MAX_BATCH_BYTES))
                    .map_err(|e| e.to_string())?;
                pending.push((info, file));
                pending.sort_by_key(|(info, _)| {
                    !matches!(info.batch_type, BatchType::MetadataEmbedded)
                });
                let mut keep = Vec::new();
                for (info, file) in std::mem::take(&mut pending) {
                    match session.decode(&mut wasm, &info, &file) {
                        Ok(Decoded::Messages(chunks)) => {
                            let mut list = to_synced(&chunks, &ids, &mut seen);
                            batches += 1;
                            messages += list.len();
                            if !list.is_empty() {
                                store_all(&store_path, &mut list);
                                if ui_tx.unbounded_send(SyncUpdate::Batch(list)).is_err() {
                                    return Err("Đã hủy đồng bộ.".into());
                                }
                            }
                            report(Progress::Receiving { batches, messages });
                        }
                        Ok(Decoded::Conversations(_)) => batches += 1,
                        Err(Sync2Error::NoSessionRecord) => keep.push((info, file)),
                        Err(err) => return Err(sync_err(err)),
                    }
                }
                pending = keep;
            }
            Update::Failed(info) => {
                return Err(format!(
                    "Điện thoại báo lỗi ở lô {} (mã {}).",
                    info.idx, info.err
                ));
            }
            Update::Ignored | Update::Duplicate => {}
        }
    }
}

impl ShellView {
    /// Start syncing old message history from the linked phone (`sync-v2`).
    pub(crate) fn start_phone_sync(&mut self, cx: &mut gpui::Context<Self>) {
        if self.phone_sync.to_worker.is_some() || cfg!(test) {
            return;
        }
        self.phone_sync.finished_ok = false;
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            self.phone_sync.progress = Some(Progress::Failed("Cần đăng nhập để đồng bộ.".into()));
            self.set_note("Cần đăng nhập để đồng bộ từ điện thoại");
            return;
        };
        let Some(out) = sync_outgoing() else {
            let msg = "Chưa kết nối realtime; hãy thử lại sau.".to_owned();
            self.phone_sync.progress = Some(Progress::Failed(msg.clone()));
            self.set_note(msg);
            return;
        };
        let (to_worker, signals) = std::sync::mpsc::channel();
        let (ui_tx, mut ui_rx) = futures::channel::mpsc::unbounded();
        self.phone_sync.to_worker = Some(to_worker);
        self.phone_sync.progress = Some(Progress::Preparing);
        RUNNING.store(true, Ordering::Relaxed);
        let _ = std::thread::Builder::new()
            .name("zalo-gpui-sync2".into())
            .spawn(move || worker(&dir, ui_tx, out, signals));

        cx.spawn(async move |this, cx| {
            while let Some(ev) = ui_rx.next().await {
                if this
                    .update(cx, |view, cx| {
                        view.apply_phone_sync_update(ev);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = this.update(cx, |view, cx| {
                view.phone_sync.to_worker = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// Cancel an active phone-sync run (drops the signal sender to end the worker loop).
    pub(crate) fn cancel_phone_sync(&mut self) {
        if self.phone_sync.to_worker.take().is_some() {
            self.phone_sync.finished_ok = false;
            self.phone_sync.progress = Some(Progress::Failed("Đã hủy đồng bộ.".into()));
            self.set_note("Đã hủy đồng bộ tin nhắn");
        }
    }

    /// Forward `cmd 590` replies and `cmd 601` controls to the running sync worker.
    pub(crate) fn on_sync_signal(&mut self, cmd: u16, data: Value) {
        let Some(to_worker) = &self.phone_sync.to_worker else {
            return;
        };
        let ok = if cmd == CMD_REQUEST_SYNC {
            to_worker.send(Signal::Reply(data)).is_ok()
        } else {
            controls_of(&data)
                .into_iter()
                .all(|c| to_worker.send(Signal::Control(c)).is_ok())
        };
        if !ok {
            self.phone_sync.to_worker = None;
        }
    }

    fn apply_phone_sync_update(&mut self, update: SyncUpdate) {
        match update {
            SyncUpdate::Status(note) => self.set_note(note),
            SyncUpdate::Batch(list) => {
                for m in list {
                    self.apply_incoming(m, false, false);
                }
            }
            SyncUpdate::Progress(p) => {
                if matches!(p, Progress::Done { .. } | Progress::Failed(_)) {
                    self.phone_sync.to_worker = None;
                    // Hide the list banner when the run finishes. Leaving
                    // `finished_ok` set kept the success row until a click.
                    // Dismissing also keeps the suggest row from coming back.
                    self.phone_sync.finished_ok = false;
                    if let Progress::Done { messages } = &p {
                        self.prefs
                            .set("syncBannerDismissed", serde_json::Value::Bool(true));
                        self.set_note(format!("Đồng bộ từ điện thoại xong: {messages} tin"));
                    }
                }
                self.phone_sync.progress = Some(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_maps_and_queries_from_server_shapes() {
        let friends = serde_json::json!([
            {"userId": "111", "globalId": "G111"},
            {"userId": "222", "globalId": "G222"}
        ]);
        let me = serde_json::json!({"data": {"userId": "999", "globalId": "G999"}});
        let groups = vec![serde_json::json!({"data": {"gridInfoMap": {
            "555": {"groupId": "555", "globalId": "GG555"}
        }}})];
        let ids = IdMaps::from_values(&friends, &me, &groups);
        assert_eq!(ids.me, "999");
        assert_eq!(ids.pc_id("G111", false).as_deref(), Some("111"));
        assert_eq!(ids.pc_id("GG555", true).as_deref(), Some("555"));
        assert_eq!(ids.pc_id("unknown", false).as_deref(), Some("unknown"));
        let parts: HashSet<String> = ids.queries().into_iter().map(|q| q.partition).collect();
        assert!(parts.contains("group/GG555"));
        assert!(parts.contains("oneone/G111"));
        assert!(
            !parts.contains("oneone/G999"),
            "no query for our own account"
        );
    }

    #[test]
    fn phone_batch_dedups_and_persists_once() {
        use zalo_sync2::proto::{Message, MessageChunk, Partition};
        let ids = IdMaps::from_values(
            &serde_json::json!([{"userId": "111", "globalId": "G111"}]),
            &serde_json::json!({"userId": "999", "globalId": "G999"}),
            &[serde_json::json!({"gridInfoMap": {"555": {"groupId": "555", "globalId": "GG555"}}})],
        );
        let msg = |sender: &str, gid: i64, ts: u64, body: &str| Message {
            sender_id: sender.into(),
            client_id: ts,
            msg_type: 0,
            timestamp: ts,
            global_id: gid,
            content: body.into(),
            ..Message::default()
        };
        let chunk = MessageChunk {
            platform: 1,
            partitions: vec![
                Partition {
                    id: "oneone/G111".into(),
                    messages: vec![
                        msg("G111", 10, 1_700_000_000_000, "tin cũ từ bạn"),
                        msg("G999", 11, 1_700_000_001_000, "tin cũ của mình"),
                    ],
                },
                Partition {
                    id: "group/GG555".into(),
                    messages: vec![msg("GX", 12, 1_700_000_002_000, "người lạ trong nhóm")],
                },
            ],
        };
        let mut seen = HashSet::new();
        let mut list = to_synced(std::slice::from_ref(&chunk), &ids, &mut seen);
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|m| m.seen_record.is_none()));
        assert!(list
            .iter()
            .any(|m| m.from_me && m.thread_id == "111" && m.text == "tin cũ của mình"));
        assert!(list.iter().any(|m| m.is_group && m.thread_id == "g_555"));

        let dir = std::env::temp_dir().join(format!("zalo-gpui-sync2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("store.db");
        store_all(&db, &mut list);
        let mut again = to_synced(&[chunk], &ids, &mut HashSet::new());
        store_all(&db, &mut again);
        let store = zalo_storage::LocalStore::open(&db).expect("open store");
        assert_eq!(store.all_messages().expect("all").len(), 3);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn req_ids_controls_and_progress_lines_work() {
        let body = with_req_id(&serde_json::json!({"data": {"syncId": "x"}}));
        assert!(body["reqId"]
            .as_str()
            .is_some_and(|id| id.starts_with("req_")));
        assert_eq!(body["data"]["syncId"], "x");

        let data = serde_json::json!({"data": {"controls": [
            {"content": {"act_type": "transfer_sync2", "act": "transfer_status", "data": "{}"}},
            {"act_type": "transfer_sync2", "act": "upload_batch", "data": {}}
        ]}});
        let list = controls_of(&data);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["act"], "transfer_status");
        assert_eq!(list[1]["act"], "upload_batch");

        assert!(progress_line(&Progress::WaitingConfirm).contains("Đồng ý"));
        assert_eq!(
            progress_line(&Progress::Done { messages: 42 }),
            "Xong: đã nhận 42 tin."
        );
    }

    #[test]
    fn sync_banner_follows_session_dismiss_and_the_running_sync() {
        use SyncBannerKind::{Hidden, Progress, Success, Suggest};
        assert_eq!(sync_banner_kind(false, false, false, false), Hidden);
        assert_eq!(sync_banner_kind(false, false, true, true), Hidden);
        assert_eq!(sync_banner_kind(true, false, false, false), Suggest);
        assert_eq!(sync_banner_kind(true, true, false, false), Hidden);
        assert_eq!(sync_banner_kind(true, true, true, false), Progress);
        assert_eq!(sync_banner_kind(true, false, true, true), Progress);
        assert_eq!(sync_banner_kind(true, false, false, true), Success);
        assert_eq!(sync_banner_kind(true, true, false, true), Hidden);
    }
}
