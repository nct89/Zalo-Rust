//! Unity launcher badge from `plugins/launcher-badge`.
//!
//! The signal is `com.canonical.Unity.LauncherEntry.Update` on
//! `/com/canonical/Unity/LauncherEntry`. The payload is a dict with `count`
//! (`int64`) and `count-visible` (`bool`).

use crate::badge::{normalize_count, normalize_count_text};
use crate::bus::session;
use crate::error::ShellError;

/// Object path the launcher plugin passes to `gdbus emit`.
pub const UNITY_OBJECT_PATH: &str = "/com/canonical/Unity/LauncherEntry";

/// Interface that owns the `Update` signal.
pub const UNITY_INTERFACE: &str = "com.canonical.Unity.LauncherEntry";

/// Signal member. The plugin's full name is `{UNITY_INTERFACE}.Update`.
pub const UNITY_SIGNAL: &str = "Update";

/// Inputs that choose which `.desktop` ids receive the count.
#[derive(Clone, Debug, Default)]
pub struct DesktopQuery {
    /// `ZALO_DESKTOP_FILE`.
    pub zalo_desktop_file: Option<String>,
    /// `GTK_DESKTOP_FILE`.
    pub gtk_desktop_file: Option<String>,
    /// `XDG_CURRENT_DESKTOP_FILE`.
    pub xdg_current_desktop_file: Option<String>,
    /// `APPIMAGE`, when the process was started from an AppImage.
    pub appimage: Option<String>,
    /// Packaged apps use `zalo.desktop`. A dev Electron tree uses `electron.desktop`.
    pub packaged: bool,
}

/// Strip `application://` and a directory, and ensure a `.desktop` suffix.
pub fn normalize_desktop_file(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_prefix = trimmed.strip_prefix("application://").unwrap_or(trimmed);
    let base = without_prefix
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_prefix);
    if base.is_empty() {
        return None;
    }
    if base.ends_with(".desktop") {
        Some(base.to_owned())
    } else {
        Some(format!("{base}.desktop"))
    }
}

/// Desktop ids in the plugin's order, without duplicates.
pub fn desktop_files(query: &DesktopQuery) -> Vec<String> {
    let mut names = Vec::new();
    names.push(query.zalo_desktop_file.clone());
    names.push(query.gtk_desktop_file.clone());
    names.push(query.xdg_current_desktop_file.clone());
    names.push(query.appimage.as_deref().and_then(appimage_desktop_file));
    names.push(Some(if query.packaged {
        "zalo.desktop".to_owned()
    } else {
        "electron.desktop".to_owned()
    }));
    names.push(Some("com.zalo.linux.desktop".to_owned()));
    names.push(Some("zalo-for-linux.desktop".to_owned()));
    names.push(Some("Zalo.desktop".to_owned()));
    names.push(Some("zalo.desktop".to_owned()));
    let mut unique = Vec::new();
    for name in names.into_iter().flatten() {
        let Some(normalized) = normalize_desktop_file(&name) else {
            continue;
        };
        if !unique.contains(&normalized) {
            unique.push(normalized);
        }
    }
    unique
}

/// Desktop ids for this process.
///
/// The Rust GUI is not the Electron dev tree, so the packaged name `zalo.desktop`
/// is used in place of `electron.desktop`.
pub fn desktop_files_from_env() -> Vec<String> {
    // Both Rust GUIs' own entries (`app_id` zalo-gui / zalo-gpui) come first so
    // dock badges work regardless of which frontend binary runs.
    let mut names = vec![
        format!("{}.desktop", crate::GPUI_APP_ID),
        format!("{}.desktop", crate::APP_ID),
    ];
    for name in desktop_files(&DesktopQuery {
        zalo_desktop_file: std::env::var("ZALO_DESKTOP_FILE").ok(),
        gtk_desktop_file: std::env::var("GTK_DESKTOP_FILE").ok(),
        xdg_current_desktop_file: std::env::var("XDG_CURRENT_DESKTOP_FILE").ok(),
        appimage: std::env::var("APPIMAGE").ok(),
        packaged: true,
    }) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Read `(12)` at the start of a title, or `N tin nhan` / `N message(s)`.
///
/// No match returns `None`. A matched zero returns `Some(0)`.
pub fn parse_title_count(title: &str) -> Option<u32> {
    if let Some(count) = leading_paren_count(title) {
        return Some(normalize_count(count));
    }
    phrase_count(title)
}

/// GVariant literal the plugin passes to `gdbus`, after the count is normalized.
pub fn unity_badge_payload(raw_count: i64) -> String {
    let count = normalize_count(raw_count);
    let visible = if count > 0 { "true" } else { "false" };
    format!("{{ 'count': <int64 {count}>, 'count-visible': <{visible}> }}")
}

/// Emit `LauncherEntry.Update` for one desktop id.
///
/// Returns an error when the session bus is missing. Unit tests do not call this.
pub fn publish_unity_badge(desktop_file: &str, raw_count: i64) -> Result<(), ShellError> {
    let count = i64::from(normalize_count(raw_count));
    let visible = count > 0;
    let mut fields = std::collections::HashMap::new();
    fields.insert("count", zbus::zvariant::Value::from(count));
    fields.insert("count-visible", zbus::zvariant::Value::from(visible));
    let application = format!("application://{desktop_file}");
    let connection = session()?;
    connection
        .emit_signal(
            None::<&str>,
            UNITY_OBJECT_PATH,
            UNITY_INTERFACE,
            UNITY_SIGNAL,
            &(application, fields),
        )
        .map_err(|error| ShellError::Bus(error.to_string()))?;
    Ok(())
}

fn appimage_desktop_file(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if name.is_empty() {
        return None;
    }
    let stem = strip_appimage_suffix(name);
    let mut cleaned = String::new();
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            cleaned.push(ch);
        } else {
            cleaned.push('-');
        }
    }
    let trimmed = cleaned.trim_matches('-');
    Some(format!("{trimmed}.desktop"))
}

fn strip_appimage_suffix(name: &str) -> &str {
    let suffix = ".appimage";
    if name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    {
        &name[..name.len() - suffix.len()]
    } else {
        name
    }
}

fn leading_paren_count(title: &str) -> Option<i64> {
    let rest = title.trim_start();
    let rest = rest.strip_prefix('(')?;
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with(')') {
        return None;
    }
    digits.parse().ok()
}

fn phrase_count(title: &str) -> Option<u32> {
    let lower = title.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() && (index == 0 || !is_word(bytes[index - 1])) {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            let digits = &lower[start..index];
            let mut cursor = index;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor == index {
                continue;
            }
            let rest = &lower[cursor..];
            for word in ["tin nhan", "message", "messages"] {
                if !rest.starts_with(word) {
                    continue;
                }
                let end = cursor + word.len();
                if end == lower.len() || !is_word(bytes[end]) {
                    return Some(normalize_count_text(digits));
                }
            }
            continue;
        }
        index += 1;
    }
    None
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_ids_follow_the_plugin_list() {
        assert_eq!(
            normalize_desktop_file("application://Foo.desktop").as_deref(),
            Some("Foo.desktop")
        );
        assert_eq!(
            normalize_desktop_file("  bar  ").as_deref(),
            Some("bar.desktop")
        );
        assert_eq!(normalize_desktop_file("   "), None);
        let files = desktop_files(&DesktopQuery {
            zalo_desktop_file: Some("application://custom.desktop".into()),
            gtk_desktop_file: None,
            xdg_current_desktop_file: None,
            appimage: Some("/opt/My App.AppImage".into()),
            packaged: true,
        });
        assert_eq!(
            files,
            vec![
                "custom.desktop".to_owned(),
                "My-App.desktop".to_owned(),
                "zalo.desktop".to_owned(),
                "com.zalo.linux.desktop".to_owned(),
                "zalo-for-linux.desktop".to_owned(),
                "Zalo.desktop".to_owned(),
            ]
        );
        let dev = desktop_files(&DesktopQuery {
            packaged: false,
            ..DesktopQuery::default()
        });
        assert_eq!(dev[0], "electron.desktop");
    }

    #[test]
    fn titles_and_payloads_match_the_plugin() {
        assert_eq!(parse_title_count("  (12) Zalo"), Some(12));
        assert_eq!(parse_title_count("(0) Zalo"), Some(0));
        assert_eq!(parse_title_count("3 tin nhan moi"), Some(3));
        assert_eq!(parse_title_count("4 Messages"), Some(4));
        assert_eq!(parse_title_count("hello"), None);
        assert_eq!(parse_title_count("x3 messages"), None);
        assert_eq!(
            unity_badge_payload(3),
            "{ 'count': <int64 3>, 'count-visible': <true> }"
        );
        assert_eq!(
            unity_badge_payload(0),
            "{ 'count': <int64 0>, 'count-visible': <false> }"
        );
        assert_eq!(UNITY_OBJECT_PATH, "/com/canonical/Unity/LauncherEntry");
        assert_eq!(
            format!("{UNITY_INTERFACE}.{UNITY_SIGNAL}"),
            "com.canonical.Unity.LauncherEntry.Update"
        );
    }
}
