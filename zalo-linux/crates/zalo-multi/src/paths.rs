//! XDG directory resolution for ZaloMulti (`paths.js` parity).
//!
//! Captures the real user `XDG_CONFIG_HOME` / `XDG_DATA_HOME` once on startup
//! (or from `ZALOMULTI_CONFIG_HOME` / `ZALOMULTI_DATA_HOME` forwarded into
//! spawned clones) so clone processes with isolated `XDG_*` still know where
//! the shared ZaloMulti config and desktop settings live.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn resolve_xdg(primary_env: &str, fallback_env: &str, rel_fallback: &str) -> PathBuf {
    for key in [primary_env, fallback_env] {
        if let Some(val) = std::env::var_os(key) {
            let p = PathBuf::from(val);
            if p.is_absolute() {
                return p;
            }
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(rel_fallback)
}

/// Real user `XDG_CONFIG_HOME` (captured once on process startup).
#[must_use]
pub fn user_config_home() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| resolve_xdg("ZALOMULTI_CONFIG_HOME", "XDG_CONFIG_HOME", ".config"))
}

/// Real user `XDG_DATA_HOME` (captured once on process startup).
#[must_use]
pub fn user_data_home() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| resolve_xdg("ZALOMULTI_DATA_HOME", "XDG_DATA_HOME", ".local/share"))
}

/// ZaloMulti application data directory (`~/.config/ZaloMulti`).
#[must_use]
pub fn app_data_dir() -> PathBuf {
    let dir = user_config_home().join("ZaloMulti");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Default root directory for clone profiles (`~/.local/share/ZaloMulti/Profiles`).
#[must_use]
pub fn default_profile_root() -> PathBuf {
    user_data_home().join("ZaloMulti").join("Profiles")
}

/// Active profile root directory (honoring `custom_path.txt` when absolute).
#[must_use]
pub fn profile_root() -> PathBuf {
    profile_root_from(&app_data_dir(), &default_profile_root())
}

/// Testable helper resolving `custom_path.txt` inside `app_dir`.
#[must_use]
pub(crate) fn profile_root_from(app_dir: &Path, default_root: &Path) -> PathBuf {
    if let Ok(raw) = std::fs::read_to_string(app_dir.join("custom_path.txt")) {
        let trimmed = raw.trim();
        let path = PathBuf::from(trimmed);
        if !trimmed.is_empty() && path.is_absolute() {
            return path;
        }
    }
    default_root.to_path_buf()
}
