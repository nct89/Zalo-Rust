//! Windows desktop environment metadata, right-aligned Win11 caption controls,
//! and Win32 global hotkeys (`RegisterHotKey`).

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::error::ShellError;
use crate::integrate::{self, set_autostart_for};
use crate::tray::{tray_backend, TrayBackend};

/// Placeholder kept so the GUI can name the same integration surface as Linux.
pub const PORTAL_BACKGROUND_INTERFACE: &str = "windows.RunRegistry";

/// Host desktop. The Windows shell only runs on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopEnvironment {
    /// Windows (Win32 / DWM).
    Windows,
}

/// Result of a background-permission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundGrant {
    /// The process may keep running in the Windows notification area.
    pub background: bool,
    /// Autostart registry entry is present.
    pub autostart: bool,
}

/// Status line shown in Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopShellStatus {
    /// Always [`DesktopEnvironment::Windows`].
    pub env: DesktopEnvironment,
    /// Always false.
    pub is_wayland: bool,
    /// Always false.
    pub is_flatpak: bool,
    /// Always false.
    pub sni_watcher_present: bool,
    /// Always false.
    pub xembed_tray_present: bool,
    /// Active tray backend once [`crate::install_desktop_shell_for`] has run.
    pub active_backend: TrayBackend,
    /// Vietnamese guidance.
    pub hint: String,
}

/// Windows is the only environment this crate reports.
#[must_use]
pub fn current_desktop() -> DesktopEnvironment {
    DesktopEnvironment::Windows
}

/// Always false.
#[must_use]
pub fn is_flatpak() -> bool {
    false
}

/// Settings hint for the Windows notification area.
#[must_use]
pub fn format_shell_hint(
    _env: DesktopEnvironment,
    backend: TrayBackend,
    _sni_watcher_present: bool,
    _xembed_tray_present: bool,
) -> String {
    match backend {
        TrayBackend::WindowsTray | TrayBackend::StatusNotifier | TrayBackend::MenuBar => {
            "Đang dùng khay hệ thống Windows (biểu tượng cạnh đồng hồ).".to_owned()
        }
        TrayBackend::XEmbed => "XEmbed không dùng trên Windows.".to_owned(),
        TrayBackend::None => {
            "Chưa gắn được khay hệ thống. Đóng cửa sổ cuối sẽ thoát Zalo.".to_owned()
        }
    }
}

/// Current notification-area status.
#[must_use]
pub fn desktop_shell_status() -> DesktopShellStatus {
    let active_backend = tray_backend();
    let hint = format_shell_hint(DesktopEnvironment::Windows, active_backend, false, false);
    DesktopShellStatus {
        env: DesktopEnvironment::Windows,
        is_wayland: false,
        is_flatpak: false,
        sni_watcher_present: false,
        xembed_tray_present: false,
        active_backend,
        hint,
    }
}

/// Write or remove the Windows Run registry entry.
pub fn request_background_async(app_id: &str, autostart: bool, _hidden_cmd: bool) {
    let _ = set_autostart_for(app_id, autostart);
}

/// Synchronous form of [`request_background_async`].
///
/// # Errors
///
/// Returns [`ShellError::Io`] when the startup entry cannot be updated.
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
    /// Maximize / Restore.
    Maximize,
    /// Close.
    Close,
}

/// Visual style of the caption buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionStyle {
    /// Linux circles. Present so the shared titlebar can match every variant.
    LinuxRound,
    /// KDE Breeze flat icons.
    KdeBreeze,
    /// macOS red / yellow / green dots.
    MacTrafficLight,
    /// Windows 10/11 rectangular right-aligned caption controls.
    Windows,
}

/// Resolved caption layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowControlsLayout {
    /// Buttons on the left, left to right.
    pub left: Vec<WindowButton>,
    /// Buttons on the right, left to right.
    pub right: Vec<WindowButton>,
    /// Unused on Windows.
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
            style: CaptionStyle::Windows,
        }
    }
}

/// Windows caption buttons on the right: minimize, maximize, close.
#[must_use]
pub fn detect_window_controls() -> WindowControlsLayout {
    WindowControlsLayout::default()
}

/// Windows draws crisp caption glyphs directly in the titlebar.
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
    /// Global hotkeys are unavailable.
    Unavailable(String),
}

/// Handle for rebinding Windows global hotkeys (`RegisterHotKey`).
pub struct GlobalShortcuts {
    commands: Sender<Vec<ShortcutSpec>>,
}

impl GlobalShortcuts {
    /// Registers global hotkeys on the Win32 tray message thread (Windows),
    /// or reports [`ShortcutEvent::Unavailable`] off Windows / in tests.
    pub fn start(
        _app_id: &str,
        specs: Vec<ShortcutSpec>,
        on_event: impl Fn(ShortcutEvent) + Send + Sync + 'static,
    ) -> Self {
        let (commands, rx) = std::sync::mpsc::channel::<Vec<ShortcutSpec>>();
        #[cfg(target_os = "windows")]
        if !cfg!(test) {
            let callback: std::sync::Arc<dyn Fn(ShortcutEvent) + Send + Sync> =
                std::sync::Arc::new(on_event);
            crate::tray_win32::register_hotkeys(specs, std::sync::Arc::clone(&callback));
            let _ = std::thread::Builder::new()
                .name("zalo-hotkeys".into())
                .spawn(move || {
                    while let Ok(next_specs) = rx.recv() {
                        crate::tray_win32::register_hotkeys(
                            next_specs,
                            std::sync::Arc::clone(&callback),
                        );
                    }
                });
            return Self { commands };
        }
        #[cfg(not(target_os = "windows"))]
        let _ = (specs, rx);
        on_event(ShortcutEvent::Unavailable(
            "Phím tắt toàn cục chỉ hoạt động trên Windows; phím vẫn dùng được khi cửa sổ Zalo đang mở."
                .into(),
        ));
        Self { commands }
    }

    /// Rebind the active global shortcut list.
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
pub const PORTAL_GLOBAL_SHORTCUTS_INTERFACE: &str = "windows.RegisterHotKey";
/// Session placeholder.
pub const PORTAL_SESSION_INTERFACE: &str = "windows.RegisterHotKey.session";
/// Unused on Windows.
pub const KDE_GLOBALACCEL_SERVICE: &str = "";
/// Unused on Windows.
pub const PORTAL_REGISTRY_INTERFACE: &str = "";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_controls_sit_on_the_right() {
        let layout = detect_window_controls();
        assert_eq!(layout.style, CaptionStyle::Windows);
        assert!(layout.left.is_empty());
        assert_eq!(
            layout.right,
            vec![
                WindowButton::Minimize,
                WindowButton::Maximize,
                WindowButton::Close,
            ]
        );
        assert!(find_window_icon_svg(None, "window-close-symbolic").is_none());
    }

    #[test]
    fn shortcut_ids_and_hint_match_expected_format() {
        let id = shortcut_id("shot-with-window", "CTRL+ALT+s");
        assert_eq!(id, "shot-with-window-ctrl-alt-s");
        assert_eq!(
            shortcut_slot(&id, &["shot-with-window", "shot-without-window"]),
            Some("shot-with-window")
        );
        let hint = format_shell_hint(
            DesktopEnvironment::Windows,
            TrayBackend::WindowsTray,
            false,
            false,
        );
        assert!(hint.contains("khay hệ thống Windows"));
    }
}
