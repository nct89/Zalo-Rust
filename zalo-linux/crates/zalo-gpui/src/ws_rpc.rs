//! WebSocket RPC multiplexer (`590`–`592`, `630`–`634`), control event routing (`601`/`603`),
//! and WASM asset discovery shared by `realtime.rs`, `sync_worker.rs`, and `backup_worker.rs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};
use zalo_backup::BackupControlEvent;
use zalo_client::listener::Frame;
use zalo_sync::SyncControlEvent;

/// Error returned by [`WsRpcHub::request`].
#[derive(Debug, Clone)]
pub enum WsRpcError {
    /// No active WebSocket connection.
    Offline,
    /// Timeout waiting for RPC reply.
    Timeout,
    /// Socket disconnected before reply arrived.
    Disconnected,
}

impl std::fmt::Display for WsRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Offline => write!(f, "NO_NETWORK: socket offline"),
            Self::Timeout => write!(f, "ERR_CONNECTION_TIMED_OUT: socket RPC timed out"),
            Self::Disconnected => write!(f, "ERR_NO_NETWORK: socket disconnected"),
        }
    }
}

struct RpcInner {
    gen: u64,
    tx: Option<mpsc::UnboundedSender<Frame>>,
    pending: HashMap<(u16, String), oneshot::Sender<Value>>,
    client_id: String,
    self_uid: String,
    remote_configs: Option<(zalo_sync::SyncRemoteConfig, zalo_backup::BackupRemoteConfig)>,
}

impl Default for RpcInner {
    fn default() -> Self {
        Self {
            gen: 0,
            tx: None,
            pending: HashMap::new(),
            client_id: String::new(),
            self_uid: String::new(),
            remote_configs: None,
        }
    }
}

/// Process-wide WebSocket RPC & control-event hub tied to the active `realtime.rs` connection.
pub struct WsRpcHub {
    inner: Mutex<RpcInner>,
    online: AtomicBool,
    req_seq: AtomicU64,
    sync_ctrl_tx: broadcast::Sender<SyncControlEvent>,
    backup_ctrl_tx: broadcast::Sender<BackupControlEvent>,
}

impl WsRpcHub {
    /// Global singleton hub.
    #[must_use]
    pub fn global() -> &'static Self {
        static HUB: OnceLock<WsRpcHub> = OnceLock::new();
        HUB.get_or_init(|| {
            let (sync_ctrl_tx, _) = broadcast::channel(256);
            let (backup_ctrl_tx, _) = broadcast::channel(64);
            Self {
                inner: Mutex::new(RpcInner::default()),
                online: AtomicBool::new(false),
                req_seq: AtomicU64::new(1),
                sync_ctrl_tx,
                backup_ctrl_tx,
            }
        })
    }

    /// Generates a unique monotonic `reqId` for `cmd` (matching Electron `getReqId`).
    #[must_use]
    pub fn next_req_id(&self, cmd: u16) -> String {
        let seq = self.req_seq.fetch_add(1, Ordering::Relaxed);
        format!("req_{cmd}_{seq}")
    }

    /// Attaches a fresh outbound channel for a new `realtime.rs` listener generation.
    #[must_use]
    pub fn attach_connection(
        &self,
        gen: u64,
        client_id: String,
        self_uid: String,
    ) -> mpsc::UnboundedReceiver<Frame> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut inner = self.inner.lock().unwrap();
        inner.gen = gen;
        inner.tx = Some(tx);
        inner.client_id = client_id;
        inner.self_uid = self_uid;
        inner.pending.clear();
        rx
    }

    /// Detaches the connection when `realtime.rs` stops or switches account.
    pub fn detach_connection(&self, gen: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.gen == gen {
            inner.tx = None;
            inner.pending.clear();
            inner.remote_configs = None;
            self.online.store(false, Ordering::SeqCst);
        }
    }

    /// Immediately clears all active connection state, cached remote configs,
    /// and session IDs on logout or account switch so a new login never inherits
    /// stale state from the previous account.
    pub fn reset_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.tx = None;
        inner.pending.clear();
        inner.client_id.clear();
        inner.self_uid.clear();
        inner.remote_configs = None;
        self.online.store(false, Ordering::SeqCst);
    }

    /// Caches the latest server-provided `sync_2_pc` and `backup_msg` configs.
    pub fn set_remote_configs(
        &self,
        sync: zalo_sync::SyncRemoteConfig,
        backup: zalo_backup::BackupRemoteConfig,
    ) {
        self.inner.lock().unwrap().remote_configs = Some((sync, backup));
    }

    /// Returns cached `(SyncRemoteConfig, BackupRemoteConfig)` if already fetched this session.
    #[must_use]
    pub fn remote_configs(
        &self,
    ) -> Option<(zalo_sync::SyncRemoteConfig, zalo_backup::BackupRemoteConfig)> {
        self.inner.lock().unwrap().remote_configs.clone()
    }

    /// Updates WebSocket readiness (`Ready` vs `Disconnected`).
    pub fn set_online(&self, online: bool) {
        self.online.store(online, Ordering::SeqCst);
        if !online {
            // Drop any in-flight RPC waiters immediately on disconnect so retry logic engages.
            let mut inner = self.inner.lock().unwrap();
            inner.pending.clear();
        }
    }

    /// Returns whether the realtime WebSocket is currently `Ready`.
    #[must_use]
    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::SeqCst)
    }

    /// Returns the active account's `client_id` (device IMEI) and `self_uid`.
    #[must_use]
    pub fn session_ids(&self) -> (String, String) {
        let inner = self.inner.lock().unwrap();
        (inner.client_id.clone(), inner.self_uid.clone())
    }

    /// Subscribes to decoded Sync 2 (`transfer_sync2` / `transfer_msg`) control events.
    #[must_use]
    pub fn subscribe_sync_controls(&self) -> broadcast::Receiver<SyncControlEvent> {
        self.sync_ctrl_tx.subscribe()
    }

    /// Subscribes to decoded Backup (`backup_pc2mb`) control events.
    #[must_use]
    pub fn subscribe_backup_controls(&self) -> broadcast::Receiver<BackupControlEvent> {
        self.backup_ctrl_tx.subscribe()
    }

    /// Sends one JSON WebSocket RPC frame (`cmd`, `sub_cmd`, `body`) and awaits its reply
    /// matched by `(cmd, req_id)` with `timeout_ms`.
    pub async fn request(
        &self,
        cmd: u16,
        sub_cmd: u8,
        req_id: &str,
        body: &Value,
        timeout_ms: u64,
    ) -> Result<Value, WsRpcError> {
        if !self.is_online() {
            return Err(WsRpcError::Offline);
        }
        let (resp_tx, resp_rx) = oneshot::channel();
        let frame = Frame::json(cmd, sub_cmd, body);
        {
            let mut inner = self.inner.lock().unwrap();
            let Some(tx) = inner.tx.clone() else {
                return Err(WsRpcError::Offline);
            };
            inner.pending.insert((cmd, req_id.to_owned()), resp_tx);
            if tx.send(frame).is_err() {
                inner.pending.remove(&(cmd, req_id.to_owned()));
                return Err(WsRpcError::Disconnected);
            }
        }
        match tokio::time::timeout(Duration::from_millis(timeout_ms), resp_rx).await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(_)) => Err(WsRpcError::Disconnected),
            Err(_) => {
                let mut inner = self.inner.lock().unwrap();
                inner.pending.remove(&(cmd, req_id.to_owned()));
                Err(WsRpcError::Timeout)
            }
        }
    }

    /// Completes a pending RPC waiter when `realtime.rs` receives a `590`–`592` or `630`–`634` frame.
    pub fn complete_rpc(&self, cmd: u16, data: Value) -> bool {
        let req_id = data
            .pointer("/data/reqId")
            .or_else(|| data.get("reqId"))
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            })
            .unwrap_or_default();

        let waiter = {
            let mut inner = self.inner.lock().unwrap();
            if !req_id.is_empty() {
                inner.pending.remove(&(cmd, req_id))
            } else {
                // Fallback: match any single pending waiter for this command if server omitted reqId.
                let key = inner
                    .pending
                    .keys()
                    .find(|(c, _)| *c == cmd)
                    .cloned();
                key.and_then(|k| inner.pending.remove(&k))
            }
        };
        if let Some(tx) = waiter {
            let _ = tx.send(data);
            true
        } else {
            false
        }
    }

    /// Parses and dispatches Sync 2 and Backup control events from a `601`/`603` frame payload.
    /// Returns any `TransferAfterLogin` events so `realtime.rs` can notify `ShellView`.
    pub fn dispatch_controls(&self, frame_data: &Value) -> Vec<SyncControlEvent> {
        let client_id = self.inner.lock().unwrap().client_id.clone();
        let now_ms = unix_now_ms();

        let sync_events = zalo_sync::parse_sync_controls(frame_data, &client_id, now_ms);
        let mut after_login = Vec::new();
        for ev in sync_events {
            if matches!(ev, SyncControlEvent::TransferAfterLogin(_)) {
                after_login.push(ev.clone());
            }
            let _ = self.sync_ctrl_tx.send(ev);
        }

        if let Some(list) = frame_data
            .pointer("/data/controls")
            .or_else(|| frame_data.get("controls"))
            .and_then(Value::as_array)
        {
            let backup_events = zalo_backup::parse_backup_controls(list);
            for ev in backup_events {
                let _ = self.backup_ctrl_tx.send(ev);
            }
        }

        after_login
    }
}

/// Shared background Tokio runtime for Sync 2 and PC → Phone Backup async pipelines.
#[must_use]
pub fn async_rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("zalo-sync-backup")
            .enable_all()
            .build()
            .expect("build sync/backup tokio runtime")
    })
}

/// Current Unix epoch timestamp in milliseconds.
#[must_use]
pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Embedded Sync 2 WASM module used as a fallback when the runtime `wasm/` asset
/// is not found on disk (e.g. standalone binary moved without `share/zalo-rust`).
pub const EMBEDDED_SYNC_WASM: &[u8] = include_bytes!(
    "../../../../assets/wasm/libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm"
);

/// Embedded Backup WASM module used as a fallback when the runtime `wasm/` asset
/// is not found on disk.
pub const EMBEDDED_BACKUP_WASM: &[u8] = include_bytes!(
    "../../../../assets/wasm/libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm"
);

/// Locates one of the bundled Electron WASM binaries (`libzproto_wasm_bg...` or
/// `libzproto_backup_wasm_bg...`) across dev, XDG data, macOS bundle, and installed system locations.
#[must_use]
pub fn locate_wasm(filename: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("ZALO_WASM_DIR") {
        candidates.push(PathBuf::from(dir).join(filename));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("assets/wasm").join(filename));
            candidates.push(dir.join("wasm").join(filename));
            #[cfg(target_os = "macos")]
            candidates.push(dir.join("../Resources/zalo-rust/wasm").join(filename));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("share/zalo-rust/wasm").join(filename));
                candidates.push(parent.join("assets/wasm").join(filename));
            }
        }
    }
    if let Some(data) = zalo_shell::data_home() {
        candidates.push(data.join("zalo-rust/wasm").join(filename));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("assets/wasm").join(filename));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../assets/wasm")
            .join(filename),
    );
    candidates.push(PathBuf::from("/usr/share/zalo-rust/wasm").join(filename));
    candidates.push(PathBuf::from("/usr/local/share/zalo-rust/wasm").join(filename));

    candidates.into_iter().find(|p| p.is_file())
}

/// Locates `filename` on disk via [`locate_wasm`], or materializes the embedded
/// fallback bytes into the user's data/temp directory so callers needing a path
/// still succeed when running a standalone binary.
#[must_use]
pub fn ensure_wasm_path(filename: &str, embedded: &[u8]) -> Option<PathBuf> {
    if let Some(path) = locate_wasm(filename) {
        return Some(path);
    }
    let base = zalo_shell::data_home()
        .map(|d| d.join("zalo-rust/wasm"))
        .unwrap_or_else(|| std::env::temp_dir().join("zalo-rust/wasm"));
    if std::fs::create_dir_all(&base).is_err() {
        return None;
    }
    let dst = base.join(filename);
    let needs_write = std::fs::metadata(&dst)
        .map(|m| m.len() != embedded.len() as u64)
        .unwrap_or(true);
    if needs_write {
        let tmp = base.join(format!(".{filename}.tmp.{}", std::process::id()));
        if std::fs::write(&tmp, embedded).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
        if std::fs::rename(&tmp, &dst).is_err() {
            let _ = std::fs::remove_file(&tmp);
            if !dst.is_file() {
                return None;
            }
        }
    }
    dst.is_file().then_some(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_sync_and_backup_wasm_instantiate() {
        zalo_sync::SyncWasm::from_bytes(EMBEDDED_SYNC_WASM)
            .expect("embedded Sync 2 wasm must instantiate");
        zalo_backup::BackupWasm::from_bytes(EMBEDDED_BACKUP_WASM)
            .expect("embedded Backup wasm must instantiate");
    }

    #[test]
    fn app_version_tag_matches_version() {
        assert!(!crate::APP_VERSION.is_empty());
        assert_eq!(crate::APP_VERSION_TAG, format!("v{}", crate::APP_VERSION));
    }
}
