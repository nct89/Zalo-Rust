//! Launch on login: `~/.config/autostart/zalo-rust.desktop`.
//!
//! Same rule as `patch-auto-launch` + `plugins/start-hidden` of the Electron
//! build: the entry starts the app with `--hidden`, so it goes to the tray.

use std::path::{Path, PathBuf};

use crate::appentry::quote_desktop_exec;
use crate::error::ShellError;
use crate::portal::{is_flatpak, request_background};

/// File name inside the autostart directory.
pub const AUTOSTART_FILE: &str = "zalo-rust.desktop";

/// `$XDG_CONFIG_HOME/autostart/zalo-rust.desktop` (or `~/.config/...`).
pub fn autostart_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("autostart").join(AUTOSTART_FILE))
}

/// Desktop entry text for `exe` using `app_id` as the icon name.
pub fn desktop_entry_for(app_id: &str, exe: &Path) -> String {
    let exec = quote_desktop_exec(exe);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Zalo\n\
         Comment=Zalo (Rust client)\n\
         Exec={exec} --hidden\n\
         Icon={app_id}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Desktop entry text for `exe`. Paths with spaces are quoted as the spec asks.
pub fn desktop_entry(exe: &Path) -> String {
    desktop_entry_for(crate::APP_ID, exe)
}

/// Whether the autostart entry exists at `path`.
pub fn autostart_enabled_at(path: &Path) -> bool {
    path.is_file()
}

/// Writes or removes the entry at `path` for `app_id`.
///
/// # Errors
///
/// Returns [`ShellError::Io`] when the directory or file cannot be written.
pub fn set_autostart_for_at(
    path: &Path,
    app_id: &str,
    exe: &Path,
    enabled: bool,
) -> Result<(), ShellError> {
    if enabled {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| ShellError::Io(e.to_string()))?;
        }
        std::fs::write(path, desktop_entry_for(app_id, exe))
            .map_err(|e| ShellError::Io(e.to_string()))
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ShellError::Io(e.to_string())),
        }
    }
}

/// Writes or removes the entry at `path`.
///
/// # Errors
///
/// Returns [`ShellError::Io`] when the directory or file cannot be written.
pub fn set_autostart_at(path: &Path, exe: &Path, enabled: bool) -> Result<(), ShellError> {
    set_autostart_for_at(path, crate::APP_ID, exe, enabled)
}

/// [`autostart_enabled_at`] for the user's autostart directory.
pub fn autostart_enabled() -> bool {
    autostart_path().is_some_and(|p| autostart_enabled_at(&p))
}

/// [`set_autostart_at`] for the running executable and `app_id`.
///
/// In a Flatpak sandbox, falls back to `org.freedesktop.portal.Background`.
///
/// # Errors
///
/// Returns [`ShellError::Io`] / [`ShellError::Bus`] when autostart cannot be updated.
pub fn set_autostart_for(app_id: &str, enabled: bool) -> Result<(), ShellError> {
    let exe = std::env::current_exe().map_err(|e| ShellError::Io(e.to_string()))?;
    if is_flatpak() {
        let exe_str = exe.to_string_lossy();
        let _ = request_background(app_id, enabled, &[exe_str.as_ref(), "--hidden"])?;
        return Ok(());
    }
    let path = autostart_path().ok_or_else(|| ShellError::Io("HOME is not set".to_owned()))?;
    set_autostart_for_at(&path, app_id, &exe, enabled)
}

/// [`set_autostart_at`] for the running executable in the user's autostart directory.
///
/// # Errors
///
/// Returns [`ShellError::Io`] when `HOME` is unset, the executable path is
/// unknown, or the file cannot be written.
pub fn set_autostart(enabled: bool) -> Result<(), ShellError> {
    set_autostart_for(crate::APP_ID, enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_starts_hidden_and_quotes_spaces() {
        let plain = desktop_entry(Path::new("/opt/zalo/zalo-gui"));
        assert!(plain.contains("Exec=/opt/zalo/zalo-gui --hidden\n"));
        let spaced = desktop_entry(Path::new("/home/a b/zalo-gui"));
        assert!(spaced.contains("Exec=\"/home/a b/zalo-gui\" --hidden\n"));
    }

    #[test]
    fn writes_and_removes_entry() {
        let dir = std::env::temp_dir().join(format!("zalo-autostart-{}", std::process::id()));
        let path = dir.join("autostart").join(AUTOSTART_FILE);
        assert!(!autostart_enabled_at(&path));
        set_autostart_at(&path, Path::new("/usr/bin/zalo-gui"), true).expect("enable");
        assert!(autostart_enabled_at(&path));
        set_autostart_at(&path, Path::new("/usr/bin/zalo-gui"), false).expect("disable");
        assert!(!autostart_enabled_at(&path));
        set_autostart_at(&path, Path::new("/usr/bin/zalo-gui"), false).expect("idempotent");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
