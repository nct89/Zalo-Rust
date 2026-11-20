//! PC → phone message backup, the original Zalo path.
//!
//! Official Zalo does not write a JSON file. It asks the server for the
//! backup config (`cmd 634`) and the backup already stored (`cmd 632`,
//! `/api/message/get_backupmsginfo` is the HTTP twin). Creating a new backup
//! is `cmd 631` (session), a local encrypted package, `cmd 630` (init with
//! `bkid` + `client_salt`), chunked POST to `/api/backuppc2mb/upload`, then
//! `cmd 633` so the phone can restore. This module speaks the read commands.
//! It does not upload a file of our own format.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{json, Value};
use zalo_client::Frame;

use crate::shell::ShellView;

/// `BACKUP_MSG.INIT`. Starts an upload of an already built package.
pub const CMD_BACKUP_INIT: u16 = 630;
/// `BACKUP_MSG.CREATE_SESSION`. Returns `bkid` and `server_nonce`.
pub const CMD_BACKUP_CREATE: u16 = 631;
/// `BACKUP_MSG.GET_METADATA`. Existing backup, or error `1001` when there is none.
pub const CMD_BACKUP_METADATA: u16 = 632;
/// `BACKUP_MSG.SIGNAL_RESTORE`. Tells the phone a backup is ready.
pub const CMD_BACKUP_SIGNAL: u16 = 633;
/// `BACKUP_MSG.GET_CONFIGS`. Server `retention_time` for the backup.
pub const CMD_BACKUP_CONFIGS: u16 = 634;

/// Server code for "no backup on this account".
const ERR_NONE: i64 = 1001;

static NEXT_REQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static WAIT: OnceLock<Mutex<Option<Sender<(u16, Value)>>>> = OnceLock::new();

fn wait_slot() -> &'static Mutex<Option<Sender<(u16, Value)>>> {
    WAIT.get_or_init(|| Mutex::new(None))
}

/// True while a settings click is waiting for `cmd 632` or `cmd 634`.
pub(crate) fn wants(cmd: u16) -> bool {
    if !matches!(cmd, CMD_BACKUP_METADATA | CMD_BACKUP_CONFIGS) {
        return false;
    }
    wait_slot().lock().ok().is_some_and(|guard| guard.is_some())
}

/// Hand a socket reply to the waiting lookup.
pub(crate) fn on_backup_reply(cmd: u16, data: Value) {
    if let Ok(guard) = wait_slot().lock() {
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send((cmd, data));
        }
    }
}

fn next_req_id() -> String {
    let n = NEXT_REQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("req_{n}")
}

/// Body of `cmd 632` and `cmd 634`: the request id only.
#[must_use]
pub fn lookup_body(req_id: &str) -> Value {
    json!({ "reqId": req_id })
}

/// Body of `cmd 630`. `metadata` is the package description (`size`,
/// `msg_count`, `thread_count`, `checksum`, …) plus `bkid` and `client_salt`.
#[must_use]
pub fn init_body(req_id: &str, metadata: &Value, bkid: &str, client_salt: &str) -> Value {
    let mut data = metadata.clone();
    if let Some(map) = data.as_object_mut() {
        map.insert("bkid".into(), Value::String(bkid.to_owned()));
        map.insert("client_salt".into(), Value::String(client_salt.to_owned()));
    }
    json!({ "reqId": req_id, "data": data })
}

impl ShellView {
    /// Ask the server whether a PC → phone backup already exists.
    ///
    /// Read-only: `cmd 634` then `cmd 632`. Does not create a session, upload,
    /// or signal the phone, and does not write a local file.
    pub(crate) fn start_pc_backup_lookup(&mut self, cx: &mut gpui::Context<Self>) {
        if cfg!(test) {
            return;
        }
        if !self.has_session {
            self.set_note("Cần đăng nhập để xem sao lưu tin nhắn trên máy chủ.");
            return;
        }
        let Some(out) = crate::realtime::sync_outgoing() else {
            self.set_note("Chưa kết nối realtime; hãy thử lại sau.");
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let Ok(mut guard) = wait_slot().lock() else {
                self.set_note("Không hỏi được trạng thái sao lưu.");
                return;
            };
            if guard.is_some() {
                self.set_note("Đang hỏi máy chủ về bản sao lưu.");
                return;
            }
            *guard = Some(tx);
        }
        let configs_id = next_req_id();
        let meta_id = next_req_id();
        if out
            .send(Frame::json(
                CMD_BACKUP_CONFIGS,
                0,
                &lookup_body(&configs_id),
            ))
            .is_err()
            || out
                .send(Frame::json(CMD_BACKUP_METADATA, 0, &lookup_body(&meta_id)))
                .is_err()
        {
            if let Ok(mut guard) = wait_slot().lock() {
                *guard = None;
            }
            self.set_note("Mất kết nối realtime; hãy thử lại khi đã kết nối.");
            return;
        }
        let (ui_tx, mut ui_rx) = futures::channel::mpsc::unbounded();
        let spawned = std::thread::Builder::new()
            .name("zalo-backup-status".into())
            .spawn(move || {
                let note = collect_status(&rx, &configs_id, &meta_id);
                if let Ok(mut guard) = wait_slot().lock() {
                    *guard = None;
                }
                let _ = ui_tx.unbounded_send(note);
            });
        if spawned.is_err() {
            if let Ok(mut guard) = wait_slot().lock() {
                *guard = None;
            }
            self.set_note("Không hỏi được trạng thái sao lưu.");
            return;
        }
        cx.spawn(async move |this, cx| {
            if let Some(note) = ui_rx.next().await {
                let _ = this.update(cx, |view, cx| {
                    view.set_note(note);
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

fn collect_status(rx: &Receiver<(u16, Value)>, configs_id: &str, meta_id: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut configs = None;
    let mut metadata = None;
    while Instant::now() < deadline && (configs.is_none() || metadata.is_none()) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left.min(Duration::from_millis(500))) {
            Ok((cmd, data)) => {
                if cmd == CMD_BACKUP_CONFIGS && reply_is(configs_id, &data) {
                    configs = Some(data);
                } else if cmd == CMD_BACKUP_METADATA && reply_is(meta_id, &data) {
                    metadata = Some(data);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    status_note(configs.as_ref(), metadata.as_ref())
}

fn reply_is(req_id: &str, data: &Value) -> bool {
    req_id_of(data).is_none_or(|id| id == req_id)
}

fn req_id_of(data: &Value) -> Option<&str> {
    data.get("reqId").and_then(Value::as_str).or_else(|| {
        data.get("data")
            .and_then(|inner| inner.get("reqId"))
            .and_then(Value::as_str)
    })
}

/// Vietnamese status line. Omits `bkid`, nonce, and checksum.
#[must_use]
pub fn status_note(configs: Option<&Value>, metadata: Option<&Value>) -> String {
    let retention = configs.and_then(retention_days);
    let Some(metadata) = metadata else {
        let mut note = "Không nhận được trạng thái sao lưu từ máy chủ.".to_owned();
        if let Some(days) = retention {
            note.push_str(&format!(" Cấu hình máy chủ giữ khoảng {days} ngày."));
        }
        return note;
    };
    let code = error_code(metadata).unwrap_or(0);
    if code == ERR_NONE || (code == 0 && backup_meta(metadata).is_none()) {
        let mut note =
            "Chưa có bản sao lưu tin nhắn trên máy chủ để khôi phục trên điện thoại.".to_owned();
        if let Some(days) = retention {
            note.push_str(&format!(" Bản mới được giữ khoảng {days} ngày."));
        }
        return note;
    }
    if code != 0 {
        return format!("Máy chủ từ chối đọc sao lưu (mã {code}).");
    }
    let Some(meta) = backup_meta(metadata) else {
        return "Máy chủ không trả thông tin bản sao lưu.".to_owned();
    };
    let size = meta.get("size").and_then(Value::as_u64).unwrap_or(0);
    let created = meta
        .get("created_time")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut note = format!("Đã có bản sao lưu trên máy chủ ({}).", fmt_bytes(size));
    if created > 0 {
        note.push_str(&format!(" Tạo ngày {}.", utc_date(created)));
    }
    note.push_str(" Khôi phục trên điện thoại trước khi hết hạn.");
    if let Some(days) = retention {
        note.push_str(&format!(" Giữ khoảng {days} ngày."));
    }
    note
}

fn error_code(reply: &Value) -> Option<i64> {
    reply
        .get("error_code")
        .and_then(Value::as_i64)
        .or_else(|| nested_i64(reply, "error_code"))
        .or_else(|| nested_i64(reply, "err"))
}

fn nested_i64(reply: &Value, key: &str) -> Option<i64> {
    reply
        .get("data")
        .and_then(|inner| inner.get(key))
        .and_then(Value::as_i64)
}

fn backup_meta(reply: &Value) -> Option<&Value> {
    let inner = reply.get("data").unwrap_or(reply);
    inner
        .get("metadata")
        .filter(|value| value.is_object())
        .or_else(|| {
            inner
                .get("data")
                .and_then(|deeper| deeper.get("metadata"))
                .filter(|value| value.is_object())
        })
}

fn retention_days(reply: &Value) -> Option<u64> {
    let inner = reply.get("data").unwrap_or(reply);
    let configs = inner
        .get("configs")
        .or_else(|| inner.get("data").and_then(|d| d.get("configs")))?;
    let raw = configs.get("retention_time").and_then(Value::as_u64)?;
    if raw == 0 {
        return None;
    }
    Some(if raw >= 86_400 { raw / 86_400 } else { raw })
}

fn fmt_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// UTC calendar date `dd/mm/yyyy` from Unix milliseconds.
fn utc_date(ms: u64) -> String {
    let z = (ms / 1000 / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{d:02}/{m:02}/{y}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_init_bodies_match_the_socket_shape() {
        assert_eq!(
            (
                CMD_BACKUP_INIT,
                CMD_BACKUP_CREATE,
                CMD_BACKUP_METADATA,
                CMD_BACKUP_SIGNAL,
                CMD_BACKUP_CONFIGS
            ),
            (630, 631, 632, 633, 634)
        );
        assert_eq!(lookup_body("req_4"), json!({"reqId": "req_4"}));
        let body = init_body(
            "req_9",
            &json!({"size": 10, "msg_count": 2, "thread_count": 1, "checksum": "ab"}),
            "bk",
            "salt",
        );
        assert_eq!(body["reqId"], "req_9");
        assert_eq!(body["data"]["bkid"], "bk");
        assert_eq!(body["data"]["client_salt"], "salt");
        assert_eq!(body["data"]["msg_count"], 2);
        assert_eq!(body["data"]["checksum"], "ab");
        assert!(body.get("bkid").is_none(), "bkid stays inside data");
    }

    #[test]
    fn status_note_hides_ids_and_reads_both_replies() {
        let configs = json!({"error_code": 0, "data": {"configs": {"retention_time": 2_592_000}, "reqId": "req_1"}});
        let none = json!({"error_code": 1001, "data": {"reqId": "req_2"}});
        let note = status_note(Some(&configs), Some(&none));
        assert!(note.contains("Chưa có bản sao lưu"));
        assert!(note.contains("30 ngày"));
        assert!(!note.contains("req_"));

        let existing = json!({"error_code": 0, "data": {"metadata": {
            "bkid": "secret-backup-id",
            "size": 5_242_880,
            "created_time": 1_700_000_000_000u64,
            "checksum": "deadbeef",
            "pc_device_name": "desk"
        }}});
        let note = status_note(Some(&configs), Some(&existing));
        assert!(note.contains("Đã có bản sao lưu"));
        assert!(note.contains("5.0 MB"));
        assert!(note.contains("14/11/2023"));
        assert!(!note.contains("secret-backup-id"));
        assert!(!note.contains("deadbeef"));
        assert_eq!(
            status_note(None, None),
            "Không nhận được trạng thái sao lưu từ máy chủ."
        );
    }

    #[test]
    fn unix_epoch_is_the_first_utc_day() {
        assert_eq!(utc_date(0), "01/01/1970");
    }
}
