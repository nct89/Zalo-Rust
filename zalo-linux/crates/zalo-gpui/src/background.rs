//! Tray mode: one realtime connection for the whole process.
//!
//! Zalo allows a single `zpw_ws` per session, so the listener is started
//! once here instead of per window. Events go to the open shell window
//! (its sender is the "sink"); while the window is closed to the tray they
//! are stored by the listener as usual, the tray badge counts them and a
//! desktop notification is shown (unless notifications are off).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;

use crate::realtime::RtEvent;

static STARTED: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Option<UnboundedSender<RtEvent>>> = Mutex::new(None);
static HIDDEN_UNREAD: AtomicU32 = AtomicU32::new(0);
static TOLD_TRAY: AtomicBool = AtomicBool::new(false);

/// The shell window receives realtime events through `tx` from now on.
/// Starts the shared listener the first time.
pub fn subscribe(dir: PathBuf, tx: UnboundedSender<RtEvent>) {
    if let Ok(mut sink) = SINK.lock() {
        *sink = Some(tx);
    }
    HIDDEN_UNREAD.store(0, Ordering::Relaxed);
    ensure_started(dir);
}

/// Start the listener without a window (`--hidden`, or after login).
pub fn ensure_started(dir: PathBuf) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let (tx, mut rx) = unbounded::<RtEvent>();
    crate::realtime::start(dir.clone(), tx);
    let _ = std::thread::Builder::new()
        .name("zalo-gpui-hub".into())
        .spawn(move || {
            futures::executor::block_on(async move {
                while let Some(event) = rx.next().await {
                    let sent = SINK
                        .lock()
                        .ok()
                        .and_then(|mut sink| {
                            let ok = sink
                                .as_ref()
                                .is_some_and(|s| s.unbounded_send(event.clone()).is_ok());
                            if !ok {
                                *sink = None;
                            }
                            ok.then_some(())
                        })
                        .is_some();
                    if !sent {
                        background_event(&dir, event);
                    }
                }
            });
            // Listener ended (session expired / re-login): the next window starts a new one.
            STARTED.store(false, Ordering::SeqCst);
        });
}

/// No window: count and notify incoming messages.
fn background_event(dir: &std::path::Path, event: RtEvent) {
    match event {
        RtEvent::Status(status) if !status.read.is_empty() => {
            let dec = status.read.len() as u32;
            let n = loop {
                let cur = HIDDEN_UNREAD.load(Ordering::Relaxed);
                let next = cur.saturating_sub(dec);
                if HIDDEN_UNREAD
                    .compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    break next;
                }
            };
            zalo_shell::set_unread_badge(i64::from(n));
        }
        RtEvent::Messages(list, true) => {
            let prefs = crate::prefs::Prefs::load(Some(dir.to_path_buf()));
            let notify = !prefs.bool("notifyOff", false);
            let sound = prefs.bool("notifySound", true);
            for m in list.iter().filter(|m| !m.from_me) {
                let n = HIDDEN_UNREAD.fetch_add(1, Ordering::Relaxed) + 1;
                zalo_shell::set_unread_badge(i64::from(n));
                if notify {
                    if sound {
                        play_notify_sound();
                    }
                    let who = if m.sender_name.is_empty() {
                        "Zalo".to_owned()
                    } else {
                        m.sender_name.clone()
                    };
                    let _ = zalo_shell::notify_desktop(&zalo_shell::DesktopNotice {
                        app_name: "Zalo".into(),
                        summary: who,
                        body: m.text.chars().take(200).collect(),
                        expire_ms: 5000,
                    });
                }
            }
        }
        _ => {}
    }
}

/// First close to the tray in this run: say where Zalo went.
pub fn tell_tray_once() {
    if TOLD_TRAY.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = zalo_shell::notify_desktop(&zalo_shell::DesktopNotice {
        app_name: "Zalo vẫn đang chạy".into(),
        summary: "Zalo vẫn đang chạy".into(),
        body: "Zalo đã thu nhỏ xuống khay hệ thống. Bấm biểu tượng Zalo để mở lại.".into(),
        expire_ms: 5000,
    });
}

/// Debounced incoming-message chime via `ffplay` (`notifySound` setting).
pub fn play_notify_sound() {
    static LAST_PLAYED_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    if cfg!(test) {
        return;
    }
    let now = crate::data::now_ms();
    let prev = LAST_PLAYED_MS.load(Ordering::Relaxed);
    if now - prev < 1_500
        || LAST_PLAYED_MS
            .compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    let sys = std::path::Path::new("/usr/share/sounds/freedesktop/stereo/message-new-instant.oga");
    let stock = crate::call::stock_sound_path("zalo_ringtone.mp3");
    let (path, cap) = if sys.is_file() {
        (sys.to_path_buf(), "2.0")
    } else if let Some(s) = stock {
        (s, "0.6")
    } else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("zalo-notify-sound".into())
        .spawn(move || {
            let _ = std::process::Command::new("ffplay")
                .args(["-nodisp", "-autoexit", "-loglevel", "quiet", "-t", cap])
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
}
