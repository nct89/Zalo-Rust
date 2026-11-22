//! Application Support paths, icons, and LaunchAgent login item.

use std::path::{Path, PathBuf};

use crate::badge::zalo_icon_png;
use crate::error::ShellError;

/// Stable bundle id used for the login item and Dock.
pub const APP_ID: &str = "zalo-gui";

/// `app_id` GPUI passes when installing the shell (`zalo-gpui`).
pub const GPUI_APP_ID: &str = "zalo-gpui";

/// Icon edges written next to the login item.
pub const ICON_SIZES: [u32; 4] = [32, 48, 128, 256];

const LAUNCH_AGENT_LABEL: &str = "io.github.nct89.ZaloRust";

/// `~/Library/Application Support`, or `$XDG_DATA_HOME` when that is set.
#[must_use]
pub fn data_home() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let path = PathBuf::from(xdg);
        if path.is_absolute() {
            return Some(path);
        }
    }
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
    })
}

/// `~/Library/LaunchAgents/io.github.nct89.ZaloRust.plist`.
#[must_use]
pub fn autostart_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
    })
}

/// Whether the login-item plist exists at `path`.
#[must_use]
pub fn autostart_enabled_at(path: &Path) -> bool {
    path.is_file()
}

/// Whether the login item for this user exists.
#[must_use]
pub fn autostart_enabled() -> bool {
    autostart_path().is_some_and(|path| autostart_enabled_at(&path))
}

/// LaunchAgent XML that runs `exe --hidden` at login.
#[must_use]
pub fn launch_agent_plist(exe: &Path) -> String {
    let program = xml_escape(&exe.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
    <string>--hidden</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
    )
}

/// Write or remove the login item at `path`.
///
/// # Errors
///
/// Returns [`ShellError::Io`] when the plist cannot be written or removed.
pub fn set_autostart_for_at(
    path: &Path,
    _app_id: &str,
    exe: &Path,
    enabled: bool,
) -> Result<(), ShellError> {
    if enabled {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| ShellError::Io(err.to_string()))?;
        }
        std::fs::write(path, launch_agent_plist(exe))
            .map_err(|err| ShellError::Io(err.to_string()))
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(ShellError::Io(err.to_string())),
        }
    }
}

/// Enable or disable launch-at-login for `app_id` using the current executable.
///
/// # Errors
///
/// Returns [`ShellError::Io`] when the plist cannot be updated, or when the
/// executable or LaunchAgents directory cannot be resolved.
pub fn set_autostart_for(_app_id: &str, enabled: bool) -> Result<(), ShellError> {
    let path = autostart_path().ok_or_else(|| ShellError::Io("missing HOME".into()))?;
    let exe = std::env::current_exe().map_err(|err| ShellError::Io(err.to_string()))?;
    set_autostart_for_at(&path, GPUI_APP_ID, &exe, enabled)
}

/// Enable or disable launch-at-login for the current executable.
///
/// # Errors
///
/// Returns [`ShellError::Io`] when the plist cannot be updated.
pub fn set_autostart(enabled: bool) -> Result<(), ShellError> {
    set_autostart_for(GPUI_APP_ID, enabled)
}

/// Write hicolor-style icons under `data` and return whether any file changed.
///
/// macOS does not use a `.desktop` file. The caller still receives `Ok(true)`
/// when the icon set was created.
///
/// # Errors
///
/// Returns [`ShellError`] when an icon cannot be encoded or written.
pub fn install_app_entry_at(data: &Path, app_id: &str, _exe: &Path) -> Result<bool, ShellError> {
    let mut wrote = false;
    for size in ICON_SIZES {
        let dir = data
            .join("icons")
            .join(format!("{size}x{size}"));
        let path = dir.join(format!("{app_id}.png"));
        let png = zalo_icon_png(size)?;
        if std::fs::read(&path).ok().as_deref() == Some(png.as_slice()) {
            continue;
        }
        std::fs::create_dir_all(&dir).map_err(|err| ShellError::Io(err.to_string()))?;
        std::fs::write(&path, png).map_err(|err| ShellError::Io(err.to_string()))?;
        wrote = true;
    }
    Ok(wrote)
}

/// Install icons for [`APP_ID`] under Application Support.
///
/// # Errors
///
/// Returns [`ShellError`] when Application Support cannot be resolved or written.
pub fn install_app_entry() -> Result<(), ShellError> {
    install_app_entry_for(APP_ID)
}

/// Install icons for `app_id` under Application Support.
///
/// # Errors
///
/// Returns [`ShellError`] when Application Support cannot be resolved or written.
pub fn install_gpui_app_entry() -> Result<(), ShellError> {
    install_app_entry_for(GPUI_APP_ID)
}

fn install_app_entry_for(app_id: &str) -> Result<(), ShellError> {
    let data = data_home().ok_or_else(|| ShellError::Io("missing HOME".into()))?;
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zalo-gpui"));
    install_app_entry_at(&data, app_id, &exe)?;
    Ok(())
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_escapes_xml_and_passes_hidden() {
        let plist = launch_agent_plist(Path::new("/Users/a&b/Zalo.app/Contents/MacOS/zalo-gpui"));
        assert!(plist.contains("--hidden"));
        assert!(plist.contains("/Users/a&amp;b/Zalo.app"));
        assert!(plist.contains(LAUNCH_AGENT_LABEL));
        assert!(!plist.contains("a&b"));
    }

    #[test]
    fn autostart_roundtrip_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("zalo-mac-agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("io.github.nct89.ZaloRust.plist");
        let exe = Path::new("/usr/local/bin/zalo-gpui");
        set_autostart_for_at(&path, GPUI_APP_ID, exe, true).expect("write");
        assert!(autostart_enabled_at(&path));
        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("--hidden"));
        set_autostart_for_at(&path, GPUI_APP_ID, exe, false).expect("remove");
        assert!(!autostart_enabled_at(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
