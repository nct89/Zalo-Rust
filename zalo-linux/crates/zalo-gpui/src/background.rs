//! Tray mode: one realtime connection for the whole process.
//!
//! Zalo allows a single `zpw_ws` per session, so the listener is started
//! once here instead of per window. Events go to the open shell window
//! (its sender is the "sink"); while the window is closed to the tray they
//! are stored by the listener as usual, the tray badge counts them and a
//! desktop notification is shown (unless notifications are off).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;

use crate::realtime::RtEvent;

struct ActiveHub {
    gen: u64,
    stop_tx: tokio::sync::watch::Sender<bool>,
}

static SESSION_GEN: AtomicU64 = AtomicU64::new(0);
static ACTIVE_HUB: Mutex<Option<ActiveHub>> = Mutex::new(None);
static SINK: Mutex<Option<UnboundedSender<RtEvent>>> = Mutex::new(None);
static HIDDEN_UNREAD: AtomicU32 = AtomicU32::new(0);
static TOLD_TRAY: AtomicBool = AtomicBool::new(false);

/// Stops the active realtime hub and listener (e.g. on logout or account switch).
/// Subsequent [`subscribe`] / [`ensure_started`] calls will start a fresh session.
pub fn stop() {
    let next_gen = SESSION_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    crate::realtime::invalidate_generation(next_gen);
    if let Ok(mut sink) = SINK.lock() {
        *sink = None;
    }
    if let Ok(mut hub) = ACTIVE_HUB.lock() {
        if let Some(active) = hub.take() {
            let _ = active.stop_tx.send(true);
        }
    }
    HIDDEN_UNREAD.store(0, Ordering::Relaxed);
    zalo_shell::set_unread_badge(0);
}

/// The shell window receives realtime events through `tx` from now on.
/// Starts the shared listener if no active session is running.
pub fn subscribe(dir: PathBuf, tx: UnboundedSender<RtEvent>) {
    if let Ok(mut sink) = SINK.lock() {
        *sink = Some(tx.clone());
    }
    if let Some((sync, backup)) = crate::ws_rpc::WsRpcHub::global().remote_configs() {
        let _ = tx.unbounded_send(RtEvent::RemoteConfigs { sync, backup });
    }
    HIDDEN_UNREAD.store(0, Ordering::Relaxed);
    ensure_started(dir);
}

/// Start the listener without a window (`--hidden`, or after login).
pub fn ensure_started(dir: PathBuf) {
    let (gen, stop_rx) = {
        let Ok(mut hub) = ACTIVE_HUB.lock() else {
            return;
        };
        if hub.is_some() {
            return;
        }
        let gen = SESSION_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        *hub = Some(ActiveHub { gen, stop_tx });
        (gen, stop_rx)
    };
    let (tx, mut rx) = unbounded::<RtEvent>();
    crate::realtime::start_with_shutdown(dir.clone(), tx, gen, stop_rx);
    let _ = std::thread::Builder::new()
        .name("zalo-gpui-hub".into())
        .spawn(move || {
            futures::executor::block_on(async move {
                while let Some(event) = rx.next().await {
                    if SESSION_GEN.load(Ordering::SeqCst) != gen {
                        break;
                    }
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
                    if !sent && SESSION_GEN.load(Ordering::SeqCst) == gen {
                        background_event(&dir, event);
                    }
                }
            });
            // Listener ended (session expired / logout / re-login): clear only if
            // we are still the active generation so a newer login is never clobbered.
            if let Ok(mut hub) = ACTIVE_HUB.lock() {
                if hub.as_ref().is_some_and(|active| active.gen == gen) {
                    *hub = None;
                }
            }
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
            let peek_guard = prefs.bool("peekGuard", false);
            let duration_secs = prefs
                .json("notifyDuration")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                })
                .unwrap_or(crate::notice::DEFAULT_DURATION_SECS);
            let expire_ms = (crate::notice::clamp_duration_secs(duration_secs) * 1000) as i32;
            let profiles = if notify {
                let path = dir.join("profiles.json");
                crate::profiles::Profiles::load(path.is_file().then_some(path.as_path()))
            } else {
                crate::profiles::Profiles::default()
            };
            for m in list.iter().filter(|m| !m.from_me) {
                let n = HIDDEN_UNREAD.fetch_add(1, Ordering::Relaxed) + 1;
                zalo_shell::set_unread_badge(i64::from(n));
                if notify {
                    if sound {
                        play_notify_sound();
                    }
                    let summary = resolve_bg_summary(&profiles, m);
                    let body: String = if peek_guard {
                        "Tin nhắn mới".to_owned()
                    } else if m.is_group && !m.sender_name.is_empty() {
                        format!("{}: {}", m.sender_name, m.text)
                            .chars()
                            .take(200)
                            .collect()
                    } else {
                        m.text.chars().take(200).collect()
                    };
                    let icon_path = resolve_bg_avatar_png(dir, &profiles, &m.thread_id, m.is_group);
                    let _ = zalo_shell::notify_desktop(&zalo_shell::DesktopNotice {
                        app_name: "Zalo".into(),
                        summary,
                        body,
                        expire_ms,
                        icon_path,
                    });
                }
            }
        }
        _ => {}
    }
}

fn bg_profile<'a>(
    profiles: &'a crate::profiles::Profiles,
    key: &str,
) -> Option<&'a crate::profiles::Profile> {
    if let Some(p) = profiles.map.get(key) {
        return Some(p);
    }
    let alt = if let Some(rest) = key.strip_prefix("g_") {
        rest.to_owned()
    } else {
        format!("g_{key}")
    };
    profiles.map.get(&alt)
}

fn resolve_bg_summary(
    profiles: &crate::profiles::Profiles,
    m: &crate::realtime::Incoming,
) -> String {
    if m.is_group {
        if let Some(p) = bg_profile(profiles, &m.thread_id) {
            let name = p.name.trim();
            if !name.is_empty() && !crate::profiles::is_id_label(&m.thread_id, name) {
                return name.to_owned();
            }
        }
        return "Nhóm".to_owned();
    }
    let alias = profiles.alias_or(&m.thread_id, "");
    if !alias.is_empty() {
        return alias.to_owned();
    }
    if let Some(p) = bg_profile(profiles, &m.thread_id) {
        if !p.name.trim().is_empty() {
            return p.name.trim().to_owned();
        }
    }
    if !m.sender_name.is_empty() {
        m.sender_name.clone()
    } else {
        "Zalo".to_owned()
    }
}

/// Produce a decoded 64×64 PNG path for `org.freedesktop.Notifications` `image-path`.
/// Uses the thread's own photo when available; for groups without a custom photo,
/// falls back to the first available collage member photo.
fn resolve_bg_avatar_png(
    dir: &std::path::Path,
    profiles: &crate::profiles::Profiles,
    thread_id: &str,
    is_group: bool,
) -> Option<String> {
    let mut url = bg_profile(profiles, thread_id)
        .map(|p| p.avatar.as_str())
        .filter(|u| crate::sidebar::is_real_avatar(u))
        .map(str::to_owned);
    if url.is_none() && is_group {
        if let Some(p) = bg_profile(profiles, thread_id) {
            url = p.member_ids.iter().take(4).find_map(|uid| {
                bg_profile(profiles, uid)
                    .map(|mp| mp.avatar.as_str())
                    .filter(|u| crate::sidebar::is_real_avatar(u))
                    .map(str::to_owned)
            });
        }
    }
    let url = url?;
    let cached = crate::media::cache_path_for(&url)?;
    if !cached.is_file() {
        let _ = crate::fetch::run_batch(
            Some(dir),
            vec![crate::fetch::Job {
                msg_id: 1,
                path: None,
                url: url.clone(),
                max_edge: 64,
            }],
        );
    }
    let bytes = std::fs::read(&cached).ok()?;
    let rgba = crate::media::decode_rgba(&bytes, 64)?;
    let out_dir = crate::media::photo_cache_dir()?;
    let out_path = out_dir.join(format!("notify-{}.png", crate::media::url_key(&url)));
    if !out_path.is_file() {
        rgba.save_with_format(&out_path, image::ImageFormat::Png)
            .ok()?;
    }
    Some(out_path.to_string_lossy().into_owned())
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
        icon_path: None,
    });
}

/// Debounced incoming-message chime played in-process (`notifySound` setting).
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
        (sys.to_path_buf(), std::time::Duration::from_secs(2))
    } else if let Some(s) = stock {
        (s, std::time::Duration::from_millis(600))
    } else {
        return;
    };
    crate::audio_play::play_detached(path, None, Some(cap));
}
