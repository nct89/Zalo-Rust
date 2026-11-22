//! Menu bar events. AppKit `NSStatusItem` is installed on macOS; other hosts only record events.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::badge::{badge_label, normalize_count};
use crate::error::ShellError;

/// Where the shell icon is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayBackend {
    /// Nothing registered yet.
    None,
    /// Linux StatusNotifier name, unused here. Kept so shared call sites match.
    StatusNotifier,
    /// Legacy X11 tray. Unused on macOS.
    XEmbed,
    /// `NSStatusItem` in the menu bar.
    MenuBar,
}

/// Clicks and menu actions the GUI polls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// Open the main window.
    Activate,
    /// Secondary click. Treated like open.
    SecondaryActivate,
    /// Context menu. The AppKit menu is native; this is the fallback.
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

/// Serializes event assertions in unit tests.
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

/// Install the menu bar for [`crate::APP_ID`].
pub fn install_desktop_shell() {
    install_desktop_shell_for(crate::integrate::APP_ID);
}

/// Install the menu bar item for `app_id`.
pub fn install_desktop_shell_for(app_id: &str) {
    if let Ok(mut guard) = state().lock() {
        guard.app_id = app_id.to_owned();
        guard.ready = true;
    }
    #[cfg(target_os = "macos")]
    if !cfg!(test) {
        crate::status_item::install();
    }
}

/// Remember mute so a later menu refresh can show it. The GUI stores the pref itself.
pub fn set_tray_muted(muted: bool) {
    if let Ok(mut guard) = state().lock() {
        guard.muted = muted;
    }
}

/// Active backend.
#[must_use]
pub fn tray_backend() -> TrayBackend {
    if state().lock().is_ok_and(|guard| guard.ready) {
        TrayBackend::MenuBar
    } else {
        TrayBackend::None
    }
}

/// `true` after [`install_desktop_shell_for`] on this process.
#[must_use]
pub fn tray_active() -> bool {
    state().lock().is_ok_and(|guard| guard.ready)
}

/// Wait until the menu bar item is marked ready, or `timeout` elapses.
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

/// Update the Dock badge and the menu-bar tooltip.
pub fn set_unread_badge(raw_count: i64) {
    let count = normalize_count(raw_count);
    if let Ok(mut guard) = state().lock() {
        guard.count = count;
    }
    let label = badge_label(count);
    #[cfg(target_os = "macos")]
    if !cfg!(test) {
        crate::status_item::set_badge(&label);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = label;
}

/// Same as [`set_unread_badge`], plus `Ok` so call sites match the Linux shell.
///
/// # Errors
///
/// This backend does not fail the badge update.
pub fn publish_unread(raw_count: i64) -> Result<(), ShellError> {
    set_unread_badge(raw_count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_marks_the_menu_bar_ready_and_queues_events() {
        let _guard = TEST_EVENT_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let _ = take_tray_events();
        install_desktop_shell_for("zalo-gpui-test");
        assert!(tray_active());
        assert_eq!(tray_backend(), TrayBackend::MenuBar);
        assert!(wait_for_tray_ready(Duration::from_millis(50)));
        record_tray_event(TrayEvent::Hide);
        assert_eq!(take_tray_events(), vec![TrayEvent::Hide]);
        set_unread_badge(4);
        assert!(publish_unread(0).is_ok());
    }
}
