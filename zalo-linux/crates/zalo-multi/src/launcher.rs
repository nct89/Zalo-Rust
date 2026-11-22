//! Isolated clone process spawner, status probe, and proxy configuration
//! (`launcher.js` parity, running native `zalo-gpui --profile <name>`).

use std::fs::OpenOptions;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths::{user_config_home, user_data_home};
use crate::profiles::{ensure_can_launch, list_profile_names, profile_dir};

/// Shared desktop configuration entries symlinked from the real user config dir
/// so clones inherit GTK/Qt themes, fonts, audio, and Vietnamese IME (`ibus`/`fcitx5`).
const SHARED_CONFIG_ENTRIES: &[&str] = &[
    "dconf",
    "fontconfig",
    "gtk-3.0",
    "gtk-4.0",
    "pulse",
    "user-dirs.dirs",
    "user-dirs.locale",
    "mimeapps.list",
    "kdeglobals",
    "ibus",
    "fcitx",
    "fcitx5",
    "mozc",
];

/// Outcome of launching (or focusing) a clone profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOutcome {
    /// Canonical profile name.
    pub name: String,
    /// Spawned PID (`0` if already running and raised via socket `SHOW`).
    pub pid: u32,
    /// `true` when an existing running instance was focused instead of spawning a duplicate.
    pub already_running: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ProxyConfig {
    #[serde(default)]
    url: String,
}

/// Deterministic, safe single-instance socket `app_id` for clone `name`.
#[must_use]
pub fn profile_app_id(name: &str) -> String {
    let hash = hex::encode(Sha256::digest(name.as_bytes()));
    format!("zalo-gpui-p-{}", &hash[..16])
}

/// Isolated `XDG_CONFIG_HOME` for clone directory `dir`.
#[must_use]
pub fn config_home(dir: &Path) -> PathBuf {
    dir.join("xdg-config")
}

/// Isolated `XDG_DATA_HOME` for clone directory `dir`.
#[must_use]
pub fn data_home(dir: &Path) -> PathBuf {
    dir.join("xdg-data")
}

/// Isolated `XDG_CACHE_HOME` for clone directory `dir`.
#[must_use]
pub fn cache_home(dir: &Path) -> PathBuf {
    dir.join("xdg-cache")
}

/// Read optional proxy URL configured for clone directory `dir` (`proxy.json`).
#[must_use]
pub fn read_proxy(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("proxy.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<ProxyConfig>(&raw).ok())
        .map(|cfg| cfg.url.trim().to_owned())
        .unwrap_or_default()
}

/// Save or clear proxy URL for clone `name` (`http://...` or `socks5://...`).
///
/// # Errors
///
/// Returns an error if the profile is missing or the URL scheme is unsupported.
pub fn save_proxy(name: &str, url: &str) -> Result<(), String> {
    let dir = profile_dir(name)?;
    if !dir.exists() {
        return Err("Không tìm thấy tài khoản!".into());
    }
    let trimmed = url.trim();
    if trimmed.is_empty() {
        let _ = std::fs::remove_file(dir.join("proxy.json"));
        return Ok(());
    }
    let valid_scheme = trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("socks5://")
        || trimmed.starts_with("socks5h://");
    if !valid_scheme {
        return Err("Proxy phải bắt đầu bằng http://, https:// hoặc socks5://".into());
    }
    let json = serde_json::to_string_pretty(&ProxyConfig {
        url: trimmed.to_owned(),
    })
    .map_err(|e| e.to_string())?;
    std::fs::write(dir.join("proxy.json"), json).map_err(|e| format!("Không lưu được proxy: {e}"))
}

fn link_shared_config(target_config: &Path) {
    let real_cfg = user_config_home();
    for entry in SHARED_CONFIG_ENTRIES {
        let source = real_cfg.join(entry);
        let link = target_config.join(entry);
        if !source.exists() {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(&link) {
            if !meta.file_type().is_symlink() {
                continue;
            }
            if std::fs::read_link(&link).ok().as_deref() == Some(source.as_path()) {
                continue;
            }
            let _ = std::fs::remove_file(&link);
        }
        let _ = symlink(&source, &link);
    }
}

fn read_recorded_pid(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join("pid.txt"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&pid| pid > 0)
}

fn pid_is_alive_zalo(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some(idx) = stat.rfind(')') else {
        return false;
    };
    let state = stat
        .get(idx + 2..)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("Z");
    if state == "Z" {
        return false;
    }
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let exe_str = exe.to_string_lossy();
    exe_str.contains("zalo-gpui")
}

/// Return `true` when clone `name` (at `dir`) is currently running.
#[must_use]
pub fn is_running(name: &str, dir: &Path) -> bool {
    let app_id = profile_app_id(name);
    if zalo_shell::ping_instance(&app_id) {
        return true;
    }
    read_recorded_pid(dir).is_some_and(pid_is_alive_zalo)
}

/// Launch clone `name` using the current `zalo-gpui` binary, or bring its window
/// to front if already running.
///
/// # Errors
///
/// Returns auth, quota, missing-profile, or spawn errors.
pub fn launch_clone(name: &str) -> Result<LaunchOutcome, String> {
    let clean = ensure_can_launch(name)?;
    let dir = profile_dir(&clean)?;
    if !dir.exists() {
        return Err("Không tìm thấy tài khoản!".into());
    }
    let app_id = profile_app_id(&clean);
    if zalo_shell::try_send_instance_command(&app_id, zalo_shell::InstanceCommand::Show) {
        return Ok(LaunchOutcome {
            name: clean,
            pid: 0,
            already_running: true,
        });
    }

    let cfg = config_home(&dir);
    let data = data_home(&dir);
    let cache = cache_home(&dir);
    std::fs::create_dir_all(&cfg).map_err(|e| format!("Không tạo được xdg-config: {e}"))?;
    std::fs::create_dir_all(&data).map_err(|e| format!("Không tạo được xdg-data: {e}"))?;
    std::fs::create_dir_all(&cache).map_err(|e| format!("Không tạo được xdg-cache: {e}"))?;
    link_shared_config(&cfg);

    let exe = std::env::current_exe().map_err(|e| format!("Không xác định được zalo-gpui: {e}"))?;
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("zalo.log"))
        .map_err(|e| format!("Không mở được tệp log clone: {e}"))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| format!("Không mở được stderr log clone: {e}"))?;

    let mut cmd = Command::new(&exe);
    cmd.arg("--profile")
        .arg(&clean)
        .env("XDG_CONFIG_HOME", &cfg)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CACHE_HOME", &cache)
        .env("ZALO_PROFILE_NAME", &clean)
        .env("ZALO_PROFILE_APP_ID", &app_id)
        .env("ZALOMULTI_CONFIG_HOME", user_config_home())
        .env("ZALOMULTI_DATA_HOME", user_data_home())
        .env("ZALO_GPUI_NO_TRAY", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err));

    let proxy = read_proxy(&dir);
    if !proxy.is_empty() {
        cmd.env("HTTP_PROXY", &proxy)
            .env("HTTPS_PROXY", &proxy)
            .env("ALL_PROXY", &proxy)
            .env("http_proxy", &proxy)
            .env("https_proxy", &proxy)
            .env("all_proxy", &proxy);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Không khởi chạy được clone \"{clean}\": {e}"))?;
    let pid = child.id();
    let _ = std::fs::write(dir.join("pid.txt"), pid.to_string());
    Ok(LaunchOutcome {
        name: clean,
        pid,
        already_running: false,
    })
}

/// Stop clone `name` gracefully via socket `QUIT`, falling back to `SIGTERM`.
///
/// # Errors
///
/// Returns an error if the profile does not exist.
pub fn stop_clone(name: &str) -> Result<bool, String> {
    let dir = profile_dir(name)?;
    let app_id = profile_app_id(name);
    let sent_quit =
        zalo_shell::try_send_instance_command(&app_id, zalo_shell::InstanceCommand::Quit);
    let pid = read_recorded_pid(&dir);
    if !sent_quit && !pid.is_some_and(pid_is_alive_zalo) {
        return Ok(false);
    }
    for _ in 0..25 {
        if !is_running(name, &dir) {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Some(pid) = pid.filter(|&p| pid_is_alive_zalo(p)) {
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
        for _ in 0..15 {
            if !pid_is_alive_zalo(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(true)
}

/// Stop all currently running clones for the signed-in ZaloMulti account.
pub fn stop_all() -> usize {
    let mut stopped = 0;
    for name in list_profile_names() {
        if stop_clone(&name).unwrap_or(false) {
            stopped += 1;
        }
    }
    stopped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_app_id_is_deterministic_and_ascii_safe() {
        let id1 = profile_app_id("Tài khoản 1");
        let id2 = profile_app_id("Tài khoản 1");
        let id3 = profile_app_id("Tài khoản 2");
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
        assert!(id1.starts_with("zalo-gpui-p-"));
        assert!(id1.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    }
}
