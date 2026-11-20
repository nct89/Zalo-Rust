//! System light/dark preference, like `patch-auto-theme` of the Electron build.
//!
//! Order: XDG portal `org.freedesktop.appearance color-scheme`, then GNOME
//! `gsettings color-scheme`, then a GTK theme name containing `dark`.
//! `None` means the desktop states no preference.

use zbus::zvariant::{OwnedValue, Value};

use crate::bus::session;

/// Portal service for settings.
pub const PORTAL_SETTINGS_INTERFACE: &str = "org.freedesktop.portal.Settings";

/// Portal value: 1 prefers dark, 2 prefers light, anything else no preference.
pub fn scheme_is_dark(value: u32) -> Option<bool> {
    match value {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// `gsettings get org.gnome.desktop.interface color-scheme` output.
pub fn gsettings_scheme_is_dark(output: &str) -> Option<bool> {
    let value = output.trim().trim_matches('\'');
    match value {
        "prefer-dark" => Some(true),
        "prefer-light" => Some(false),
        _ => None,
    }
}

/// `gsettings get org.gnome.desktop.interface gtk-theme` output.
pub fn gtk_theme_is_dark(output: &str) -> Option<bool> {
    let value = output.trim().trim_matches('\'');
    if value.is_empty() {
        None
    } else {
        Some(value.to_ascii_lowercase().contains("dark"))
    }
}

fn unwrap_u32(value: &Value<'_>) -> Option<u32> {
    match value {
        Value::Value(inner) => unwrap_u32(inner),
        Value::U32(n) => Some(*n),
        Value::I32(n) => u32::try_from(*n).ok(),
        Value::U8(n) => Some(u32::from(*n)),
        _ => None,
    }
}

fn portal_scheme() -> Option<u32> {
    let connection = session().ok()?;
    for method in ["ReadOne", "Read"] {
        let Ok(reply) = connection.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some(PORTAL_SETTINGS_INTERFACE),
            method,
            &("org.freedesktop.appearance", "color-scheme"),
        ) else {
            continue;
        };
        let Ok(value) = reply.body().deserialize::<OwnedValue>() else {
            continue;
        };
        if let Some(n) = unwrap_u32(&value) {
            return Some(n);
        }
    }
    None
}

fn gsettings(key: &str) -> Option<String> {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", key])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reads the desktop preference. Opens the session bus and may run `gsettings`.
pub fn system_prefers_dark() -> Option<bool> {
    if let Some(dark) = portal_scheme().and_then(scheme_is_dark) {
        return Some(dark);
    }
    if let Some(dark) = gsettings("color-scheme").and_then(|o| gsettings_scheme_is_dark(&o)) {
        return Some(dark);
    }
    gsettings("gtk-theme").and_then(|o| gtk_theme_is_dark(&o))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_portal_and_gsettings_values() {
        assert_eq!(scheme_is_dark(1), Some(true));
        assert_eq!(scheme_is_dark(2), Some(false));
        assert_eq!(scheme_is_dark(0), None);
        assert_eq!(gsettings_scheme_is_dark("'prefer-dark'\n"), Some(true));
        assert_eq!(gsettings_scheme_is_dark("'default'\n"), None);
        assert_eq!(gtk_theme_is_dark("'Adwaita-dark'\n"), Some(true));
        assert_eq!(gtk_theme_is_dark("'Breeze'\n"), Some(false));
        let nested = Value::Value(Box::new(Value::Value(Box::new(Value::U32(1)))));
        assert_eq!(unwrap_u32(&nested), Some(1));
    }

    /// Reads the real desktop. Run with `cargo test -p zalo-shell -- --ignored`.
    #[test]
    #[ignore = "needs a desktop session bus"]
    fn reads_live_portal() {
        let scheme = portal_scheme();
        eprintln!(
            "portal color-scheme: {scheme:?}, prefers dark: {:?}",
            system_prefers_dark()
        );
        assert!(scheme.is_some());
    }
}
