//! Windows system tray state and event queue.
//!
//! On Windows, [`crate::tray_win32`] runs a dedicated hidden Win32 message loop
//! owning the `Shell_NotifyIconW` icon, dynamic unread badge `HICON`, native
//! context menu, and global hotkeys. Off Windows this module tracks state for
//! workspace compilation and unit tests.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::badge::normalize_count;
use crate::error::ShellError;

/// Where the shell icon is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayBackend {
    /// Nothing registered yet.
    None,
    /// Linux StatusNotifier name (also kept so shared call sites match).
    StatusNotifier,
    /// Legacy X11 tray. Unused on Windows.
    XEmbed,
    /// macOS MenuBar. Unused on Windows.
    MenuBar,
    /// Native Win32 notification area (`Shell_NotifyIconW`).
    WindowsTray,
}

/// Clicks and menu actions the GUI polls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// Open or focus the main window.
    Activate,
    /// Secondary click. Treated like open.
    SecondaryActivate,
    /// Context menu fallback.
    ContextMenu,
    /// Hide the main window.
    Hide,
    /// Toggle notification mute.
    ToggleMute,
    /// Quit.
    Quit,
}

struct TrayState {
    app_id: String,
    count: u32,
    muted: bool,
    ready: bool,
}

fn state() -> &'static Mutex<TrayState> {
    static STATE: OnceLock<Mutex<TrayState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(TrayState {
            app_id: crate::integrate::GPUI_APP_ID.to_owned(),
            count: 0,
            muted: false,
            ready: false,
        })
    })
}

fn events() -> &'static Mutex<Vec<TrayEvent>> {
    static EVENTS: OnceLock<Mutex<Vec<TrayEvent>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
pub(crate) static TEST_EVENT_LOCK: Mutex<()> = Mutex::new(());

/// App id passed to the most recent install.
#[must_use]
pub fn active_app_id() -> String {
    state()
        .lock()
        .map(|guard| guard.app_id.clone())
        .unwrap_or_else(|_| crate::integrate::GPUI_APP_ID.to_owned())
}

/// Whether notification mute is currently set on the tray.
#[cfg(target_os = "windows")]
#[must_use]
pub(crate) fn tray_muted() -> bool {
    state().lock().is_ok_and(|guard| guard.muted)
}

/// Queue one event for the GUI.
pub fn record_tray_event(event: TrayEvent) {
    if let Ok(mut queue) = events().lock() {
        queue.push(event);
    }
}

/// Drain queued events.
#[must_use]
pub fn take_tray_events() -> Vec<TrayEvent> {
    events()
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

/// Install the Windows tray for [`crate::APP_ID`].
pub fn install_desktop_shell() {
    install_desktop_shell_for(crate::integrate::APP_ID);
}

/// Install the Windows tray icon for `app_id`.
pub fn install_desktop_shell_for(app_id: &str) {
    if let Ok(mut guard) = state().lock() {
        guard.app_id = app_id.to_owned();
        guard.ready = true;
    }
    #[cfg(target_os = "windows")]
    if !cfg!(test) {
        crate::tray_win32::install();
    }
}

/// Remember mute state so the native right-click menu reflects it.
pub fn set_tray_muted(muted: bool) {
    if let Ok(mut guard) = state().lock() {
        guard.muted = muted;
    }
}

/// Active backend.
#[must_use]
pub fn tray_backend() -> TrayBackend {
    if state().lock().is_ok_and(|guard| guard.ready) {
        TrayBackend::WindowsTray
    } else {
        TrayBackend::None
    }
}

/// `true` after [`install_desktop_shell_for`] on this process.
#[must_use]
pub fn tray_active() -> bool {
    state().lock().is_ok_and(|guard| guard.ready)
}

/// Wait until the tray icon is marked ready, or `timeout` elapses.
#[must_use]
pub fn wait_for_tray_ready(timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if tray_active() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Update the tray icon unread badge and tooltip.
pub fn set_unread_badge(raw_count: i64) {
    let count = normalize_count(raw_count);
    if let Ok(mut guard) = state().lock() {
        guard.count = count;
    }
    #[cfg(target_os = "windows")]
    if !cfg!(test) {
        crate::tray_win32::set_unread(count);
    }
}

/// Update the unread badge and return the normalized count.
///
/// # Errors
///
/// Currently infallible; returns `Result` to match the Linux shell API.
pub fn publish_unread(raw_count: i64) -> Result<u32, ShellError> {
    let count = normalize_count(raw_count);
    set_unread_badge(i64::from(count));
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_event_queue_roundtrips() {
        let _guard = TEST_EVENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_tray_events();
        record_tray_event(TrayEvent::Activate);
        record_tray_event(TrayEvent::ToggleMute);
        assert_eq!(
            take_tray_events(),
            vec![TrayEvent::Activate, TrayEvent::ToggleMute]
        );
        assert!(take_tray_events().is_empty());
    }

    #[test]
    fn install_marks_windows_tray_ready() {
        install_desktop_shell_for(crate::integrate::GPUI_APP_ID);
        assert!(tray_active());
        assert_eq!(tray_backend(), TrayBackend::WindowsTray);
        assert_eq!(active_app_id(), crate::integrate::GPUI_APP_ID);
        assert_eq!(publish_unread(15).expect("unread"), 15);
    }
}
