//! Desktop screenshot capture via `org.freedesktop.portal.Screenshot` and
//! native Linux screenshot utilities (`plugins/screenshot` parity).
//!
//! Unit tests exercise URI decoding, portal handle paths, and command tables
//! without opening the D-Bus session bus.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use zbus::zvariant::{OwnedValue, Value};

use crate::bus::session;
use crate::error::ShellError;

/// Freedesktop portal bus name.
pub const PORTAL_SERVICE: &str = "org.freedesktop.portal.Desktop";

/// Object path for the desktop portal.
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// Screenshot interface on [`PORTAL_PATH`].
pub const PORTAL_SCREENSHOT_INTERFACE: &str = "org.freedesktop.portal.Screenshot";

/// Request interface that emits `Response(u32, a{sv})`.
pub const PORTAL_REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

/// CLI tools for a **full-desktop** capture, no region picker.
///
/// Zalo PC crops and annotates in its own overlay. `{output}` is the PNG path.
/// Region flags (`-r`, `-s`, `gui`) are not used here: they open Spectacle /
/// Flameshot instead of Zalo's overlay.
pub const SCREENSHOT_TOOLS: &[(&str, &[&str])] = &[
    ("spectacle", &["-b", "-n", "-f", "-o", "{output}"]),
    ("grim", &["-t", "png", "{output}"]),
    ("gnome-screenshot", &["-f", "{output}"]),
    ("xfce4-screenshooter", &["-f", "{output}"]),
    ("flameshot", &["full", "--raw"]),
    ("scrot", &["{output}"]),
];

/// Compute the expected XDG Desktop Portal request object path for `unique_name` and `token`.
///
/// The leading `:` of `unique_name` (for example `:1.42`) is stripped and `.` is replaced with `_`.
#[must_use]
pub fn portal_request_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/request/{sender}/{token}")
}

/// Parse a `file://` URI returned by the Screenshot portal into a local [`PathBuf`].
///
/// Percent-encoded UTF-8 sequences (`%20`, `%C3%A0`, …) are decoded. Non-`file://`
/// URIs or invalid UTF-8 sequences return `None`.
#[must_use]
pub fn parse_file_uri(uri: &str) -> Option<PathBuf> {
    let trimmed = uri.trim();
    let rest = trimmed.strip_prefix("file://")?;
    let raw_path = if let Some(after_localhost) = rest.strip_prefix("localhost/") {
        format!("/{after_localhost}")
    } else if rest.starts_with('/') {
        rest.to_owned()
    } else {
        return None;
    };
    let decoded = percent_decode(&raw_path)?;
    Some(PathBuf::from(decoded))
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            if idx + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_val(bytes[idx + 1])?;
            let lo = hex_val(bytes[idx + 2])?;
            out.push((hi << 4) | lo);
            idx += 3;
        } else {
            out.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(out).ok()
}

const fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Capture the full desktop without a region picker.
///
/// CLI fullscreen tools run first so KDE Spectacle is not opened in region
/// mode. The portal is called with `interactive=false` only as a fallback.
///
/// Returns the raw PNG/JPEG image bytes.
///
/// # Errors
///
/// Returns [`ShellError`] when neither a local tool nor the portal produced an
/// image.
pub fn capture_screenshot() -> Result<Vec<u8>, ShellError> {
    capture_fullscreen()
}

/// Full-desktop capture used by Zalo's in-app crop overlay.
pub fn capture_fullscreen() -> Result<Vec<u8>, ShellError> {
    if let Ok(bytes) = capture_via_cli_tools() {
        return Ok(bytes);
    }
    capture_via_portal(false)
}

/// Interactive region/window capture via `org.freedesktop.portal.Screenshot`
/// (`interactive=true`, matching Electron `plugins/screenshot/portal.py`).
///
/// # Errors
///
/// Returns [`ShellError`] when the portal is unavailable or the user cancels.
pub fn capture_interactive() -> Result<Vec<u8>, ShellError> {
    capture_via_portal(true)
}

fn capture_via_portal(interactive: bool) -> Result<Vec<u8>, ShellError> {
    let connection = session()?;
    let unique = connection
        .unique_name()
        .ok_or_else(|| ShellError::Bus("session bus has no unique name".into()))?
        .as_str()
        .to_owned();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("zalo_screenshot_{}_{nanos}", std::process::id());
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
    options.insert("interactive", Value::from(interactive));

    connection
        .call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(PORTAL_SCREENSHOT_INTERFACE),
            "Screenshot",
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
            "screenshot cancelled or rejected (status {status})"
        )));
    }
    let uri_val = results
        .get("uri")
        .ok_or_else(|| ShellError::Bus("portal response missing uri".into()))?;
    let uri_str: &str = uri_val
        .downcast_ref()
        .map_err(|error| ShellError::Bus(error.to_string()))?;
    let path = parse_file_uri(uri_str)
        .ok_or_else(|| ShellError::Image(format!("invalid portal file URI: {uri_str}")))?;
    let bytes = fs::read(&path).map_err(|error| ShellError::Image(error.to_string()))?;
    let _ = fs::remove_file(&path);
    Ok(bytes)
}

fn capture_via_cli_tools() -> Result<Vec<u8>, ShellError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_path = std::env::temp_dir().join(format!(
        "zalo-screenshot-{}-{nanos}.png",
        std::process::id()
    ));
    let temp_str = temp_path.to_string_lossy().into_owned();

    for (program, template_args) in SCREENSHOT_TOOLS {
        if !command_exists(program) {
            continue;
        }
        let args: Vec<String> = template_args
            .iter()
            .map(|arg| {
                if *arg == "{output}" {
                    temp_str.clone()
                } else {
                    (*arg).to_owned()
                }
            })
            .collect();
        let output = Command::new(program).args(&args).output();
        let Ok(output) = output else {
            continue;
        };
        if !output.status.success() {
            let _ = fs::remove_file(&temp_path);
            continue;
        }
        if *program == "flameshot" && !output.stdout.is_empty() {
            return Ok(output.stdout);
        }
        if let Ok(bytes) = fs::read(&temp_path) {
            let _ = fs::remove_file(&temp_path);
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
    }
    let _ = fs::remove_file(&temp_path);
    Err(ShellError::Image(
        "không tìm thấy XDG Screenshot Portal hoặc công cụ chụp màn hình".into(),
    ))
}

fn command_exists(program: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(program))
        .any(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_portal_request_path_like_flatpak_helper() {
        assert_eq!(
            portal_request_path(":1.128", "zalo_screenshot_abc"),
            "/org/freedesktop/portal/desktop/request/1_128/zalo_screenshot_abc"
        );
        assert_eq!(
            portal_request_path("1.9", "tok"),
            "/org/freedesktop/portal/desktop/request/1_9/tok"
        );
    }

    #[test]
    fn decodes_file_uris_with_spaces_and_vietnamese_utf8() {
        assert_eq!(
            parse_file_uri("file:///home/user/Pictures/%E1%BA%A2nh%20ch%E1%BB%A5p.png"),
            Some(PathBuf::from("/home/user/Pictures/Ảnh chụp.png"))
        );
        assert_eq!(
            parse_file_uri("file://localhost/tmp/capture.png\n"),
            Some(PathBuf::from("/tmp/capture.png"))
        );
        assert_eq!(parse_file_uri("https://example.com/a.png"), None);
        assert_eq!(parse_file_uri("file:///bad%2"), None);
        assert_eq!(parse_file_uri("file:///bad%ZZ.png"), None);
    }

    #[test]
    fn screenshot_tool_table_is_non_empty() {
        assert!(SCREENSHOT_TOOLS.len() >= 5);
        assert_eq!(SCREENSHOT_TOOLS[0].0, "spectacle");
        assert!(SCREENSHOT_TOOLS[0].1.contains(&"-f"));
        assert!(!SCREENSHOT_TOOLS[0]
            .1
            .iter()
            .any(|a| *a == "-r" || *a == "-rbn"));
    }
}
