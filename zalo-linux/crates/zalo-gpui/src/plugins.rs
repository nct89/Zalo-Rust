//! Linux desktop plugins used by the GPUI shell.
//!
//! Screenshot is silent fullscreen (`spectacle -b -n -f` / grim / …). Tray,
//! notify, autostart and Unity badge stay uninstalled until the user asks.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// GPUI never calls `install_desktop_shell` on startup.
#[allow(dead_code)]
pub const AUTO_TRAY: bool = false;

/// Region capture fallback for Wayland/Flatpak or non-X11 sessions, matching
/// Electron `plugins/screenshot/index.js`: tries desktop region tools
/// (`spectacle`, `flameshot`, `grim+slurp`, `gnome-screenshot`,
/// `xfce4-screenshooter`, `mate-screenshot`, `scrot`) and finally
/// `org.freedesktop.portal.Screenshot` (`interactive=true`).
/// `Ok(None)` when the user cancels. Blocking — run it off the UI thread.
///
/// # Errors
///
/// Returns a short message when neither a region tool nor the XDG portal is available.
pub fn capture_region() -> Result<Option<PathBuf>, String> {
    use std::process::Command;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let out = std::env::temp_dir().join(format!("zalo-gpui-shot-{nanos}.png"));
    let has = |tool: &str| {
        Command::new("sh")
            .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
            .status()
            .is_ok_and(|s| s.success())
    };
    let o = out.display().to_string();
    let ran = if has("spectacle") {
        // Native plugin: `spectacle -rbcn` (region, background, clipboard, no GUI).
        Some(
            Command::new("spectacle")
                .args(["-r", "-b", "-c", "-n", "-o", &o])
                .status(),
        )
    } else if has("flameshot") {
        Some(Command::new("flameshot").args(["gui", "-p", &o]).status())
    } else if has("grim") && has("slurp") {
        Some(
            Command::new("sh")
                .args(["-c", &format!("g=$(slurp) && grim -g \"$g\" '{o}'")])
                .status(),
        )
    } else if has("gnome-screenshot") {
        Some(
            Command::new("gnome-screenshot")
                .args(["-a", "-f", &o])
                .status(),
        )
    } else if has("xfce4-screenshooter") {
        Some(
            Command::new("xfce4-screenshooter")
                .args(["-r", "-s", &o])
                .status(),
        )
    } else if has("mate-screenshot") {
        Some(
            Command::new("mate-screenshot")
                .args(["-a", "-f", &o])
                .status(),
        )
    } else if has("scrot") {
        Some(Command::new("scrot").args(["-s", &o]).status())
    } else {
        None
    };

    if let Some(status_res) = ran {
        status_res.map_err(|e| e.to_string())?;
        // Cancelled pickers exit 0 without a file on some tools.
        return Ok(std::fs::metadata(&out)
            .ok()
            .filter(|m| m.len() > 0)
            .map(|_| out));
    }

    // Fallback: XDG Desktop Portal interactive screenshot (`portal.py` parity).
    match zalo_shell::capture_interactive() {
        Ok(bytes) if !bytes.is_empty() => {
            std::fs::write(&out, &bytes).map_err(|e| e.to_string())?;
            Ok(Some(out))
        }
        Ok(_) => Ok(None),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("cancelled") || msg.contains("status 1") {
                Ok(None)
            } else {
                Err(format!(
                    "Không tìm thấy công cụ chụp vùng hoặc XDG Screenshot Portal ({msg})"
                ))
            }
        }
    }
}

/// Image picker. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns a short message when no portal and no dialog tool is available.
pub fn pick_photo() -> Result<Option<PathBuf>, String> {
    zalo_shell::pick_image(crate::color::str_vi("STR_SEND_PHOTO", "Gửi hình ảnh"))
        .map_err(|e| e.to_string())
}

/// Several images. Empty when the user cancels.
///
/// # Errors
///
/// Returns a short message when no portal and no dialog tool is available.
pub fn pick_photos() -> Result<Vec<PathBuf>, String> {
    zalo_shell::pick_images(crate::color::str_vi("STR_SEND_PHOTO", "Gửi hình ảnh"))
        .map_err(|e| e.to_string())
}

/// File picker. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns a short message when no portal and no dialog tool is available.
pub fn pick_file() -> Result<Option<PathBuf>, String> {
    zalo_shell::pick_file(crate::color::str_vi("STR_SEND_FILE", "Gửi File"))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_tools_are_fullscreen_only() {
        assert!(!AUTO_TRAY);
        for (bin, args) in zalo_shell::SCREENSHOT_TOOLS {
            assert!(
                !args
                    .iter()
                    .any(|flag| *flag == "-r" || *flag == "-s" || *flag == "gui"),
                "{bin} must not open a region picker"
            );
        }
        let filter = zalo_shell::zenity_filter("Ảnh");
        assert!(filter.contains("*.png"));
        assert!(zalo_shell::parse_dialog_output("relative.png").is_none());
    }
}
