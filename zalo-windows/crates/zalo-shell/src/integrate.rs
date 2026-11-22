//! Windows AppData paths, app icons, and `HKCU\...\Run` startup registration.

use std::path::{Path, PathBuf};

use crate::badge::zalo_icon_png;
use crate::error::ShellError;

/// Stable app id used for startup and shell integration.
pub const APP_ID: &str = "zalo-gui";

/// `app_id` GPUI passes when installing the shell (`zalo-gpui`).
pub const GPUI_APP_ID: &str = "zalo-gpui";

/// Icon edges written under the user's data folder.
pub const ICON_SIZES: [u32; 4] = [32, 48, 128, 256];

#[cfg(target_os = "windows")]
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const RUN_VALUE_NAME: &str = "ZaloRust";

/// `%LOCALAPPDATA%` (or `%APPDATA%` fallback, or `XDG_DATA_HOME` if overridden).
#[must_use]
pub fn data_home() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let path = PathBuf::from(xdg);
        if path.is_absolute() {
            return Some(path);
        }
    }
    for key in ["LOCALAPPDATA", "APPDATA"] {
        if let Some(val) = std::env::var_os(key) {
            let path = PathBuf::from(val);
            if path.is_absolute() {
                return Some(path);
            }
        }
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join("AppData").join("Local"))
}

/// Marker path used by file-based `*_at` helpers (and fallback off Windows).
#[must_use]
pub fn autostart_path() -> Option<PathBuf> {
    data_home().map(|dir| dir.join("zalo-rust").join("autostart.cmd"))
}

/// Command line written to the Windows `Run` registry value.
#[must_use]
pub fn autostart_command(exe: &Path) -> String {
    format!("\"{}\" --hidden", exe.display())
}

/// Whether the autostart marker file exists at `path`.
#[must_use]
pub fn autostart_enabled_at(path: &Path) -> bool {
    path.is_file()
}

/// Whether launch-at-login is currently enabled for this Windows user.
#[must_use]
pub fn autostart_enabled() -> bool {
    #[cfg(target_os = "windows")]
    if !cfg!(test) {
        return windows_registry::CURRENT_USER
            .open(RUN_KEY_PATH)
            .and_then(|key| key.get_string(RUN_VALUE_NAME))
            .is_ok_and(|val| !val.trim().is_empty());
    }
    autostart_path().is_some_and(|path| autostart_enabled_at(&path))
}

/// Write or remove an autostart file at `path` (used by tests and fallback).
///
/// # Errors
///
/// Returns [`ShellError::Io`] when the file cannot be written or removed.
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
        std::fs::write(path, autostart_command(exe))
            .map_err(|err| ShellError::Io(err.to_string()))
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(ShellError::Io(err.to_string())),
        }
    }
}

/// Enable or disable launch-at-login for `app_id` using `HKCU\...\Run` on Windows.
///
/// # Errors
///
/// Returns [`ShellError::Io`] when the registry key or file cannot be updated.
pub fn set_autostart_for(_app_id: &str, enabled: bool) -> Result<(), ShellError> {
    let exe = std::env::current_exe().map_err(|err| ShellError::Io(err.to_string()))?;
    #[cfg(target_os = "windows")]
    if !cfg!(test) {
        if enabled {
            let key = windows_registry::CURRENT_USER
                .create(RUN_KEY_PATH)
                .map_err(|err| ShellError::Io(err.to_string()))?;
            key.set_string(RUN_VALUE_NAME, autostart_command(&exe))
                .map_err(|err| ShellError::Io(err.to_string()))?;
        } else if let Ok(key) = windows_registry::CURRENT_USER.open(RUN_KEY_PATH) {
            let _ = key.remove_value(RUN_VALUE_NAME);
        }
        return Ok(());
    }
    let path = autostart_path().ok_or_else(|| ShellError::Io("missing LOCALAPPDATA".into()))?;
    set_autostart_for_at(&path, GPUI_APP_ID, &exe, enabled)
}

/// Enable or disable launch-at-login for the current executable.
///
/// # Errors
///
/// Returns [`ShellError::Io`] when startup registration fails.
pub fn set_autostart(enabled: bool) -> Result<(), ShellError> {
    set_autostart_for(GPUI_APP_ID, enabled)
}

/// Write icons under `data/icons/<size>x<size>/<app_id>.png`.
///
/// # Errors
///
/// Returns [`ShellError`] when an icon cannot be encoded or written.
pub fn install_app_entry_at(data: &Path, app_id: &str, _exe: &Path) -> Result<bool, ShellError> {
    let mut wrote = false;
    for size in ICON_SIZES {
        let dir = data.join("icons").join(format!("{size}x{size}"));
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

/// Install icons for [`APP_ID`] under the user's data directory.
///
/// # Errors
///
/// Returns [`ShellError`] when the directory cannot be resolved or written.
pub fn install_app_entry() -> Result<(), ShellError> {
    let data = data_home().ok_or_else(|| ShellError::Io("missing LOCALAPPDATA".into()))?;
    let exe = std::env::current_exe().map_err(|err| ShellError::Io(err.to_string()))?;
    let _ = install_app_entry_at(&data, APP_ID, &exe)?;
    Ok(())
}

/// Install icons for [`GPUI_APP_ID`] under the user's data directory.
///
/// # Errors
///
/// Returns [`ShellError`] when the directory cannot be resolved or written.
pub fn install_gpui_app_entry() -> Result<(), ShellError> {
    let data = data_home().ok_or_else(|| ShellError::Io("missing LOCALAPPDATA".into()))?;
    let exe = std::env::current_exe().map_err(|err| ShellError::Io(err.to_string()))?;
    let _ = install_app_entry_at(&data, GPUI_APP_ID, &exe)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_command_quotes_exe_and_passes_hidden() {
        let cmd = autostart_command(Path::new(r"C:\Program Files\Zalo\zalo-gpui.exe"));
        assert_eq!(cmd, "\"C:\\Program Files\\Zalo\\zalo-gpui.exe\" --hidden");
    }

    #[test]
    fn set_autostart_for_at_creates_and_removes_marker() {
        let dir = std::env::temp_dir().join(format!("zalo-win-autostart-{}", std::process::id()));
        let marker = dir.join("autostart.cmd");
        let exe = Path::new("zalo-gpui.exe");
        set_autostart_for_at(&marker, GPUI_APP_ID, exe, true).expect("enable");
        assert!(autostart_enabled_at(&marker));
        set_autostart_for_at(&marker, GPUI_APP_ID, exe, false).expect("disable");
        assert!(!autostart_enabled_at(&marker));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
