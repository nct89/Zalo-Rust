//! Desktop environment diagnostics and `org.freedesktop.portal.Background`
//! integration for tray-less environments (stock GNOME Wayland) and Flatpak.
//!
//! Unit tests exercise environment detection and diagnostic formatting without
//! touching the D-Bus session bus.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use zbus::zvariant::{OwnedValue, Value};

use crate::bus::{register_host_portal_app, session, watcher_present};
use crate::error::ShellError;
use crate::screenshot::{
    portal_request_path, PORTAL_PATH, PORTAL_REQUEST_INTERFACE, PORTAL_SERVICE,
};
use crate::tray::{tray_backend, TrayBackend};

/// Portal interface for background execution and autostart permissions.
pub const PORTAL_BACKGROUND_INTERFACE: &str = "org.freedesktop.portal.Background";

/// Recognized Linux desktop families for tray compatibility diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopEnvironment {
    /// GNOME Shell (stock Wayland lacks native SNI without an extension).
    Gnome,
    /// KDE Plasma (native SNI + DBusMenu).
    Kde,
    /// Wlroots compositors (Sway, Hyprland, River, Niri; SNI via bar e.g. Waybar).
    Wlroots,
    /// XFCE Desktop (SNI plugin or X11 Notification Area).
    Xfce,
    /// Linux Mint Cinnamon / XApp.
    Cinnamon,
    /// MATE Desktop.
    Mate,
    /// Other desktop / window manager (i3, bspwm, Openbox, LXQt, etc.).
    Other,
}

/// Result returned by [`request_background`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundGrant {
    /// Desktop allows background execution without an open window.
    pub background: bool,
    /// Desktop allows launching automatically on login.
    pub autostart: bool,
}

/// Current desktop shell integration status shown in Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopShellStatus {
    /// Detected desktop family.
    pub env: DesktopEnvironment,
    /// True when `WAYLAND_DISPLAY` is set.
    pub is_wayland: bool,
    /// True when running inside a Flatpak sandbox (`/.flatpak-info` or `FLATPAK_ID`).
    pub is_flatpak: bool,
    /// Whether `org.kde.StatusNotifierWatcher` is currently owned on the session bus.
    pub sni_watcher_present: bool,
    /// Whether an X11 `_NET_SYSTEM_TRAY_S<screen>` manager is present (`tray-xembed`).
    pub xembed_tray_present: bool,
    /// Currently active tray backend.
    pub active_backend: TrayBackend,
    /// Human-readable Vietnamese status/guidance line.
    pub hint: String,
}

/// Detect desktop environment from raw `XDG_CURRENT_DESKTOP` / `DESKTOP_SESSION` strings.
#[must_use]
pub fn detect_desktop_from(xdg_current_desktop: &str, desktop_session: &str) -> DesktopEnvironment {
    let combined = format!("{xdg_current_desktop}:{desktop_session}").to_ascii_lowercase();
    if combined.contains("kde") || combined.contains("plasma") {
        DesktopEnvironment::Kde
    } else if combined.contains("gnome")
        || combined.contains("ubuntu")
        || combined.contains("unity")
    {
        DesktopEnvironment::Gnome
    } else if combined.contains("hyprland")
        || combined.contains("sway")
        || combined.contains("wlroots")
        || combined.contains("river")
        || combined.contains("niri")
        || combined.contains("wayfire")
    {
        DesktopEnvironment::Wlroots
    } else if combined.contains("xfce") {
        DesktopEnvironment::Xfce
    } else if combined.contains("cinnamon") || combined.contains("x-cinnamon") {
        DesktopEnvironment::Cinnamon
    } else if combined.contains("mate") {
        DesktopEnvironment::Mate
    } else {
        DesktopEnvironment::Other
    }
}

/// Current process desktop environment.
#[must_use]
pub fn current_desktop() -> DesktopEnvironment {
    let xdg = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let sess = std::env::var("DESKTOP_SESSION").unwrap_or_default();
    detect_desktop_from(&xdg, &sess)
}

/// True when running inside a Flatpak sandbox.
#[must_use]
pub fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists()
}

/// Format user-facing status & fallback guidance for tray integration.
#[must_use]
pub fn format_shell_hint(
    env: DesktopEnvironment,
    backend: TrayBackend,
    sni_watcher_present: bool,
    xembed_tray_present: bool,
) -> String {
    match backend {
        TrayBackend::StatusNotifier => "Đang dùng khay StatusNotifier (SNI + DBusMenu)".to_owned(),
        TrayBackend::XEmbed => "Đang dùng khay X11 XEmbed (_NET_SYSTEM_TRAY)".to_owned(),
        TrayBackend::None => {
            if sni_watcher_present {
                "Đang chờ kết nối lại khay StatusNotifier…".to_owned()
            } else if xembed_tray_present {
                "Phát hiện khay X11 XEmbed nhưng bản build chưa bật feature tray-xembed.".to_owned()
            } else if env == DesktopEnvironment::Gnome {
                "GNOME thuần ẩn khay hệ thống: hãy dùng menu chuột phải trên Dock hoặc cài AppIndicator Extension."
                    .to_owned()
            } else if env == DesktopEnvironment::Wlroots {
                "Chưa thấy thanh tray SNI (vd: Waybar tray module). Đóng cửa sổ sẽ thoát app."
                    .to_owned()
            } else {
                "Chưa phát hiện khay hệ thống. Dùng Quicklist trên Dock hoặc thông báo để mở lại."
                    .to_owned()
            }
        }
    }
}

/// Inspect current tray & environment capability.
#[must_use]
pub fn desktop_shell_status() -> DesktopShellStatus {
    let env = current_desktop();
    let is_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let is_flatpak = is_flatpak();
    let sni_watcher_present = watcher_present();
    let xembed_tray_present = xembed_available_now();
    let active_backend = tray_backend();
    let hint = format_shell_hint(
        env,
        active_backend,
        sni_watcher_present,
        xembed_tray_present,
    );
    DesktopShellStatus {
        env,
        is_wayland,
        is_flatpak,
        sni_watcher_present,
        xembed_tray_present,
        active_backend,
        hint,
    }
}

fn xembed_available_now() -> bool {
    #[cfg(feature = "tray-xembed")]
    {
        crate::xembed::xembed_tray_present()
    }
    #[cfg(not(feature = "tray-xembed"))]
    {
        false
    }
}

/// Request background persistence / autostart via `org.freedesktop.portal.Background`.
///
/// Works on GNOME 44+ (shows under Quick Settings -> Background Apps) and Flatpak.
///
/// # Errors
///
/// Returns [`ShellError::Bus`] when the portal is unavailable or rejects the request.
pub fn request_background(
    app_id: &str,
    autostart: bool,
    commandline: &[&str],
) -> Result<BackgroundGrant, ShellError> {
    let connection = session()?;
    register_host_portal_app(&connection, app_id);
    let unique = connection
        .unique_name()
        .ok_or_else(|| ShellError::Bus("session bus has no unique name".into()))?
        .as_str()
        .to_owned();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("zalo_bg_{}_{nanos}", std::process::id());
    let expected_path = portal_request_path(&unique, &token);

    let request_proxy = zbus::blocking::Proxy::new(
        &connection,
        PORTAL_SERVICE,
        expected_path.as_str(),
        PORTAL_REQUEST_INTERFACE,
    )
    .map_err(|error| ShellError::Bus(error.to_string()))?;
    let mut signals = request_proxy
        .receive_signal("Response")
        .map_err(|error| ShellError::Bus(error.to_string()))?;

    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert(
        "reason",
        Value::from("Chạy nền để nhận tin nhắn và cuộc gọi Zalo"),
    );
    options.insert("autostart", Value::from(autostart));
    options.insert("dbus-activatable", Value::from(false));
    if !commandline.is_empty() {
        let cmd: Vec<String> = commandline.iter().map(|s| (*s).to_owned()).collect();
        options.insert("commandline", Value::from(cmd));
    }

    connection
        .call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(PORTAL_BACKGROUND_INTERFACE),
            "RequestBackground",
            &("", options),
        )
        .map_err(|error| ShellError::Bus(error.to_string()))?;

    let message = signals
        .next()
        .ok_or_else(|| ShellError::Bus("portal Response stream closed".into()))?;
    let (status, results): (u32, HashMap<String, OwnedValue>) = message
        .body()
        .deserialize()
        .map_err(|error| ShellError::Bus(error.to_string()))?;
    if status != 0 {
        return Err(ShellError::Bus(format!(
            "background portal cancelled or rejected (status {status})"
        )));
    }
    let background = results
        .get("background")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(true);
    let autostart_granted = results
        .get("autostart")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(autostart);
    Ok(BackgroundGrant {
        background,
        autostart: autostart_granted,
    })
}

/// Fire-and-forget portal registration so GNOME 44+ lists Zalo under Background Apps.
pub fn request_background_async(app_id: &str, autostart: bool, hidden_cmd: bool) {
    let app_id = app_id.to_owned();
    let _ = std::thread::Builder::new()
        .name("zalo-portal-bg".into())
        .spawn(move || {
            let exe = std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| app_id.clone());
            if hidden_cmd {
                let _ = request_background(&app_id, autostart, &[&exe, "--hidden"]);
            } else {
                let _ = request_background(&app_id, autostart, &[&exe]);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_major_desktops_cleanly() {
        assert_eq!(
            detect_desktop_from("KDE", "plasma"),
            DesktopEnvironment::Kde
        );
        assert_eq!(
            detect_desktop_from("ubuntu:GNOME", "ubuntu"),
            DesktopEnvironment::Gnome
        );
        assert_eq!(
            detect_desktop_from("GNOME", "gnome-wayland"),
            DesktopEnvironment::Gnome
        );
        assert_eq!(
            detect_desktop_from("Hyprland", "hyprland"),
            DesktopEnvironment::Wlroots
        );
        assert_eq!(detect_desktop_from("sway", ""), DesktopEnvironment::Wlroots);
        assert_eq!(
            detect_desktop_from("XFCE", "xfce"),
            DesktopEnvironment::Xfce
        );
        assert_eq!(detect_desktop_from("i3", "i3"), DesktopEnvironment::Other);
    }

    #[test]
    fn formats_hints_for_tray_and_no_tray_desktops() {
        assert!(format_shell_hint(
            DesktopEnvironment::Kde,
            TrayBackend::StatusNotifier,
            true,
            false
        )
        .contains("StatusNotifier"));
        assert!(
            format_shell_hint(DesktopEnvironment::Other, TrayBackend::XEmbed, false, true)
                .contains("XEmbed")
        );
        assert!(
            format_shell_hint(DesktopEnvironment::Gnome, TrayBackend::None, false, false)
                .contains("Dock")
        );
    }
}
