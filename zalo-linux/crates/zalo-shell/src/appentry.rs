//! Desktop entry and hicolor icons so docks and task managers show the Zalo
//! logo.
//!
//! On Wayland the compositor finds a window's icon through its `app_id`: the
//! dock looks up `<app_id>.desktop` and that file's `Icon=` in the icon theme.
//! [`install_app_entry`] writes the logo into
//! `$XDG_DATA_HOME/icons/hicolor/<n>x<n>/apps/<app_id>.png` and, only when no
//! entry exists yet, a `<app_id>.desktop` that starts this executable. An
//! existing desktop file the user wrote is left alone.

use std::path::{Path, PathBuf};

use crate::badge::zalo_icon_png;
use crate::error::ShellError;

/// Wayland `app_id` / X11 `WM_CLASS` of the Rust GUI; matches `zalo-gui.desktop`.
pub const APP_ID: &str = "zalo-gui";

/// Wayland `app_id` of the GPUI client. The dock looks up `zalo-gpui.desktop`.
pub const GPUI_APP_ID: &str = "zalo-gpui";

/// Icon sizes written into the hicolor theme.
pub const ICON_SIZES: [u32; 4] = [32, 48, 128, 256];

/// `$XDG_DATA_HOME`, or `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

const MANAGED_COMMENT: &str = "Comment=Zalo (Rust client)";
const QUICKLIST_LINE: &str = "Actions=open;toggle-mute;quit;";

/// Quote an executable path according to the Desktop Entry specification.
pub(crate) fn quote_desktop_exec(exe: &Path) -> String {
    let raw = exe.to_string_lossy();
    if raw.contains([' ', '"', '\\', '`', '$']) {
        let escaped = raw
            .replace('\\', "\\\\\\\\")
            .replace('"', "\\\\\"")
            .replace('`', "\\\\`")
            .replace('$', "\\\\$");
        format!("\"{escaped}\"")
    } else {
        raw.into_owned()
    }
}

/// Text of the desktop entry for `exe`, including Dock/Dash quicklist actions.
pub fn app_desktop_entry(app_id: &str, exe: &Path) -> String {
    let exec = quote_desktop_exec(exe);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Zalo\n\
         {MANAGED_COMMENT}\n\
         Exec={exec}\n\
         Icon={app_id}\n\
         Terminal=false\n\
         StartupNotify=true\n\
         StartupWMClass={app_id}\n\
         Categories=Network;InstantMessaging;\n\
         {QUICKLIST_LINE}\n\n\
         [Desktop Action open]\n\
         Name=Mở Zalo\n\
         Exec={exec} --show\n\n\
         [Desktop Action toggle-mute]\n\
         Name=Tắt/Bật thông báo\n\
         Exec={exec} --toggle-mute\n\n\
         [Desktop Action quit]\n\
         Name=Thoát Zalo\n\
         Exec={exec} --quit\n"
    )
}

fn io(err: impl std::fmt::Display) -> ShellError {
    ShellError::Io(err.to_string())
}

/// Writes the icons under `data` and the desktop entry if missing. Returns
/// `true` when anything was written (the caller may then refresh caches).
///
/// # Errors
///
/// Returns [`ShellError`] when a directory or file cannot be written.
pub fn install_app_entry_at(data: &Path, app_id: &str, exe: &Path) -> Result<bool, ShellError> {
    let mut wrote = false;
    for size in ICON_SIZES {
        let dir = data
            .join("icons/hicolor")
            .join(format!("{size}x{size}"))
            .join("apps");
        let path = dir.join(format!("{app_id}.png"));
        let png = zalo_icon_png(size)?;
        if std::fs::read(&path).ok().as_deref() == Some(png.as_slice()) {
            continue;
        }
        std::fs::create_dir_all(&dir).map_err(io)?;
        std::fs::write(&path, png).map_err(io)?;
        wrote = true;
    }
    let apps = data.join("applications");
    let desktop = apps.join(format!("{app_id}.desktop"));
    let should_write = if !desktop.exists() {
        true
    } else {
        // Upgrade older auto-generated entries to include Dock Quicklist actions,
        // while preserving any custom .desktop file written by the user.
        std::fs::read_to_string(&desktop)
            .map(|existing| {
                existing.contains(MANAGED_COMMENT) && !existing.contains(QUICKLIST_LINE)
            })
            .unwrap_or(false)
    };
    if should_write {
        std::fs::create_dir_all(&apps).map_err(io)?;
        std::fs::write(&desktop, app_desktop_entry(app_id, exe)).map_err(io)?;
        wrote = true;
    }
    Ok(wrote)
}

/// [`install_app_entry_at`] for the user's data directory and this executable.
/// When files changed, asks KDE (`kbuildsycoca6`/`5`) to reload; a missing tool
/// is ignored.
///
/// # Errors
///
/// Returns [`ShellError`] when `HOME` is unset or a file cannot be written.
pub fn install_app_entry() -> Result<(), ShellError> {
    install_app_entry_for(APP_ID)
}

/// [`install_app_entry`] for the GPUI window id `zalo-gpui`.
///
/// # Errors
///
/// Returns [`ShellError`] when `HOME` is unset or a file cannot be written.
pub fn install_gpui_app_entry() -> Result<(), ShellError> {
    install_app_entry_for(GPUI_APP_ID)
}

fn install_app_entry_for(app_id: &str) -> Result<(), ShellError> {
    let data = data_home().ok_or_else(|| io("HOME is not set"))?;
    let exe = std::env::current_exe().map_err(io)?;
    if install_app_entry_at(&data, app_id, &exe)? {
        for tool in ["kbuildsycoca6", "kbuildsycoca5"] {
            if std::process::Command::new(tool)
                .arg("--noincremental")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok()
            {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_icons_and_keeps_an_existing_entry() {
        if crate::badge::ZALO_ICON_PNG.is_none() {
            return; // built without the logo asset
        }
        let dir = std::env::temp_dir().join(format!("zalo-appentry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let exe = Path::new("/opt/zalo/zalo-gui");
        assert!(install_app_entry_at(&dir, APP_ID, exe).expect("first"));
        let icon = dir.join("icons/hicolor/256x256/apps/zalo-gui.png");
        let png = std::fs::read(&icon).expect("icon");
        assert_eq!(&png[1..4], b"PNG");
        let entry =
            std::fs::read_to_string(dir.join("applications/zalo-gui.desktop")).expect("entry");
        assert!(entry.contains("Icon=zalo-gui\n"));
        assert!(entry.contains("StartupWMClass=zalo-gui\n"));
        assert!(entry.contains("Actions=open;toggle-mute;quit;\n"));
        assert!(entry.contains("[Desktop Action open]\n"));
        assert!(entry.contains("Exec=/opt/zalo/zalo-gui --show\n"));
        assert!(entry.contains("Exec=/opt/zalo/zalo-gui --quit\n"));
        assert!(
            !install_app_entry_at(&dir, APP_ID, exe).expect("again"),
            "nothing new"
        );
        // Legacy auto-generated entry without Actions= is upgraded in place.
        std::fs::write(
            dir.join("applications/zalo-gui.desktop"),
            "[Desktop Entry]\nName=Zalo\nComment=Zalo (Rust client)\nExec=/opt/zalo/zalo-gui\n",
        )
        .expect("legacy");
        assert!(install_app_entry_at(&dir, APP_ID, exe).expect("upgrade"));
        let upgraded =
            std::fs::read_to_string(dir.join("applications/zalo-gui.desktop")).expect("upgraded");
        assert!(upgraded.contains("Actions=open;toggle-mute;quit;\n"));
        // Custom user entries without MANAGED_COMMENT are preserved untouched.
        std::fs::write(dir.join("applications/zalo-gui.desktop"), "custom").expect("custom");
        install_app_entry_at(&dir, APP_ID, exe).expect("keep");
        assert_eq!(
            std::fs::read_to_string(dir.join("applications/zalo-gui.desktop")).expect("read"),
            "custom"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
