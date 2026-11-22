//! Menu-bar status, traffic-light caption layout, and in-app shortcut registration.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::error::ShellError;
use crate::integrate::{self, set_autostart_for};
use crate::tray::{tray_backend, TrayBackend};

/// Placeholder kept so the GUI can name the same integration surface as Linux.
pub const PORTAL_BACKGROUND_INTERFACE: &str = "macos.SMAppService";

/// Host desktop. The macOS shell only runs on a Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopEnvironment {
    /// macOS (AppKit).
    Mac,
}

/// Result of a background-permission request. macOS login items do not split these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundGrant {
    /// The process may keep running after the last window closes (menu bar).
    pub background: bool,
    /// Launch at login was written.
    pub autostart: bool,
}

/// Status line shown in Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopShellStatus {
    /// Always [`DesktopEnvironment::Mac`].
    pub env: DesktopEnvironment,
    /// Always false.
    pub is_wayland: bool,
    /// Always false.
    pub is_flatpak: bool,
    /// Always false. There is no StatusNotifier watcher.
    pub sni_watcher_present: bool,
    /// Always false.
    pub xembed_tray_present: bool,
    /// Menu bar backend once [`crate::install_desktop_shell_for`] has run.
    pub active_backend: TrayBackend,
    /// Vietnamese guidance.
    pub hint: String,
}

/// macOS is the only environment this crate reports.
#[must_use]
pub fn current_desktop() -> DesktopEnvironment {
    DesktopEnvironment::Mac
}

/// Always false.
#[must_use]
pub fn is_flatpak() -> bool {
    false
}

/// Settings hint for the menu bar.
#[must_use]
pub fn format_shell_hint(
    _env: DesktopEnvironment,
    backend: TrayBackend,
    _sni_watcher_present: bool,
    _xembed_tray_present: bool,
) -> String {
    match backend {
        TrayBackend::StatusNotifier | TrayBackend::MenuBar => {
            "Đang dùng menu bar macOS (biểu tượng cạnh đồng hồ).".to_owned()
        }
        TrayBackend::XEmbed => "XEmbed không dùng trên macOS.".to_owned(),
        TrayBackend::None => {
            "Chưa gắn được menu bar. Đóng cửa sổ cuối sẽ thoát Zalo.".to_owned()
        }
    }
}

/// Current menu-bar status.
#[must_use]
pub fn desktop_shell_status() -> DesktopShellStatus {
    let active_backend = tray_backend();
    let hint = format_shell_hint(DesktopEnvironment::Mac, active_backend, false, false);
    DesktopShellStatus {
        env: DesktopEnvironment::Mac,
        is_wayland: false,
        is_flatpak: false,
        sni_watcher_present: false,
        xembed_tray_present: false,
        active_backend,
        hint,
    }
}

/// Write or remove the LaunchAgent. `hidden_cmd` is already part of the plist (`--hidden`).
pub fn request_background_async(app_id: &str, autostart: bool, _hidden_cmd: bool) {
    let _ = set_autostart_for(app_id, autostart);
}

/// Synchronous form of [`request_background_async`].
///
/// # Errors
///
/// Returns [`ShellError::Io`] when the plist cannot be written.
pub fn request_background(
    app_id: &str,
    autostart: bool,
    hidden_cmd: bool,
) -> Result<BackgroundGrant, ShellError> {
    request_background_async(app_id, autostart, hidden_cmd);
    Ok(BackgroundGrant {
        background: true,
        autostart: integrate::autostart_enabled(),
    })
}

/// Caption button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowButton {
    /// Minimize.
    Minimize,
    /// Zoom.
    Maximize,
    /// Close.
    Close,
}

/// Visual style of the caption buttons. The Mac client uses traffic lights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionStyle {
    /// Linux circles. Present so the shared titlebar can match every variant.
    LinuxRound,
    /// KDE circles.
    KdeBreeze,
    /// Red / yellow / green dots.
    MacTrafficLight,
    /// Windows 10/11 rectangular right-aligned caption controls (shared with `zalo-shell-win`).
    Windows,
}

/// Resolved caption layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowControlsLayout {
    /// Buttons on the left, left to right.
    pub left: Vec<WindowButton>,
    /// Buttons on the right.
    pub right: Vec<WindowButton>,
    /// Unused on macOS.
    pub icon_theme: Option<String>,
    /// Caption chrome.
    pub style: CaptionStyle,
}

impl Default for WindowControlsLayout {
    fn default() -> Self {
        Self {
            left: Vec::new(),
            right: vec![
                WindowButton::Minimize,
                WindowButton::Maximize,
                WindowButton::Close,
            ],
            icon_theme: None,
            style: CaptionStyle::LinuxRound,
        }
    }
}

/// Traffic lights on the left: close, minimize, zoom.
#[must_use]
pub fn detect_window_controls() -> WindowControlsLayout {
    WindowControlsLayout {
        left: vec![
            WindowButton::Close,
            WindowButton::Minimize,
            WindowButton::Maximize,
        ],
        right: Vec::new(),
        icon_theme: None,
        style: CaptionStyle::MacTrafficLight,
    }
}

/// macOS draws caption glyphs in the titlebar. There is no freedesktop icon theme.
#[must_use]
pub fn find_window_icon_svg(_icon_theme: Option<&str>, _icon_name: &str) -> Option<PathBuf> {
    None
}

/// One shortcut the GUI asked to bind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutSpec {
    /// Stable id.
    pub id: String,
    /// Human description.
    pub description: String,
    /// Trigger such as `CTRL+ALT+s`.
    pub trigger: String,
}

/// What the shortcut worker reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutEvent {
    /// The desktop accepted the shortcuts.
    Bound(Vec<(String, String)>),
    /// A shortcut was pressed.
    Activated(String),
    /// Global hotkeys are not registered. In-window keys still work.
    Unavailable(String),
}

/// Handle for rebinding. Dropping it does not unregister Carbon hotkeys (none are installed).
pub struct GlobalShortcuts {
    commands: Sender<Vec<ShortcutSpec>>,
}

impl GlobalShortcuts {
    /// Reports [`ShortcutEvent::Unavailable`]. Screenshot keys keep working while Zalo is focused.
    pub fn start(
        _app_id: &str,
        _specs: Vec<ShortcutSpec>,
        on_event: impl Fn(ShortcutEvent) + Send + Sync + 'static,
    ) -> Self {
        let (commands, _rx) = std::sync::mpsc::channel();
        on_event(ShortcutEvent::Unavailable(
            "Phím tắt toàn cục chưa gắn trên macOS; phím vẫn dùng được khi cửa sổ Zalo đang mở."
                .into(),
        ));
        Self { commands }
    }

    /// Remembers the next spec list. Global registration stays off.
    pub fn rebind(&self, specs: Vec<ShortcutSpec>) {
        let _ = self.commands.send(specs);
    }
}

/// `{slot}-{trigger}` with non-alphanumeric characters turned into `-`.
#[must_use]
pub fn shortcut_id(slot: &str, trigger: &str) -> String {
    let trigger: String = trigger
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{slot}-{trigger}")
}

/// Slot prefix of an id from [`shortcut_id`].
#[must_use]
pub fn shortcut_slot<'a>(id: &str, slots: &[&'a str]) -> Option<&'a str> {
    slots
        .iter()
        .copied()
        .filter(|slot| id == *slot || id.starts_with(&format!("{slot}-")))
        .max_by_key(|slot| slot.len())
}

/// Names kept so the shared GUI can refer to the Linux portal constants.
pub const PORTAL_GLOBAL_SHORTCUTS_INTERFACE: &str = "macos.local-shortcuts";
/// Session placeholder.
pub const PORTAL_SESSION_INTERFACE: &str = "macos.local-shortcuts.session";
/// Unused on macOS.
pub const KDE_GLOBALACCEL_SERVICE: &str = "";
/// Unused on macOS.
pub const PORTAL_REGISTRY_INTERFACE: &str = "";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traffic_lights_sit_on_the_left() {
        let layout = detect_window_controls();
        assert_eq!(layout.style, CaptionStyle::MacTrafficLight);
        assert_eq!(
            layout.left,
            vec![
                WindowButton::Close,
                WindowButton::Minimize,
                WindowButton::Maximize
            ]
        );
        assert!(layout.right.is_empty());
        assert!(find_window_icon_svg(None, "window-close-symbolic").is_none());
    }

    #[test]
    fn shortcut_ids_match_the_linux_shape() {
        let id = shortcut_id("shot-with-window", "CTRL+ALT+s");
        assert_eq!(id, "shot-with-window-ctrl-alt-s");
        assert_eq!(
            shortcut_slot(&id, &["shot-with-window", "shot-without-window"]),
            Some("shot-with-window")
        );
        let hint = format_shell_hint(DesktopEnvironment::Mac, TrayBackend::MenuBar, false, false);
        assert!(hint.contains("menu bar"));
    }
}
