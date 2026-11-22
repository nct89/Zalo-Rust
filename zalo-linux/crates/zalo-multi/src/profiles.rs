//! Clone profile directory management & quota rules (`profiles.js` parity).

use std::path::{Path, PathBuf};

use crate::account::{effective_max, require_logged_in, session_uid, NOT_LOGGED_IN};
use crate::paths::profile_root;

const INVALID_NAME: &str = "Tên tài khoản không hợp lệ.";
const OWNER_FILE: &str = ".zalomulti-account";

const RESERVED_NAMES: &[&str] = &[
    "zaloruntimes",
    "master_zalo_cache",
    "zaloweb",
    "extension",
    "accounts",
];

const DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

const FORBIDDEN_CHARS: &[char] = &[
    '\\', '/', ':', '*', '?', '"', '<', '>', '|', '\'', '%', '&', '^', '!', '`', '#', ';',
];

/// Quota error message shown when a user reaches their account clone limit.
#[must_use]
pub fn limit_error(max: usize) -> String {
    format!("Tài khoản của bạn chỉ hỗ trợ tối đa {max} clone.")
}

/// Validate and trim a clone profile name (exact parity with `canonicalProfileName`).
///
/// # Errors
///
/// Returns [`INVALID_NAME`] when empty, >64 chars, reserved, device name, or contains forbidden chars.
pub fn canonical_profile_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    let count = trimmed.chars().count();
    if trimmed.is_empty() || count > 64 {
        return Err(INVALID_NAME.into());
    }
    if trimmed.starts_with('.') || trimmed.ends_with('.') || trimmed.contains("..") {
        return Err(INVALID_NAME.into());
    }
    let lower = trimmed.to_lowercase();
    if RESERVED_NAMES.contains(&lower.as_str()) {
        return Err(INVALID_NAME.into());
    }
    let stem_upper = trimmed
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if DEVICE_NAMES.contains(&stem_upper.as_str()) {
        return Err(INVALID_NAME.into());
    }
    for c in trimmed.chars() {
        let is_control = c.is_control();
        let is_other_space = c.is_whitespace() && c != ' ';
        if is_control || is_other_space || FORBIDDEN_CHARS.contains(&c) {
            return Err(INVALID_NAME.into());
        }
    }
    Ok(trimmed.to_owned())
}

/// Join `name` directly under `root`, guaranteeing no directory traversal.
///
/// # Errors
///
/// Returns [`INVALID_NAME`] if `name` is invalid or escapes `root`.
pub fn contained_join(root: &Path, name: &str) -> Result<PathBuf, String> {
    let clean = canonical_profile_name(name)?;
    let joined = root.join(&clean);
    if joined.parent() != Some(root) {
        return Err(INVALID_NAME.into());
    }
    Ok(joined)
}

fn stamp_owner(dir: &Path, uid: i64) {
    let _ = std::fs::write(dir.join(OWNER_FILE), uid.to_string());
}

/// Migrate legacy root-level profiles into `<base>/accounts/<uid>` once per account.
pub(crate) fn migrate_legacy_into(base: &Path, dest: &Path, uid: i64) {
    let _ = std::fs::create_dir_all(dest);
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let uid_text = uid.to_string();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if canonical_profile_name(&name).ok().as_deref() != Some(name.as_str()) {
            continue;
        }
        let from = entry.path();
        if crate::launcher::is_running(&name, &from) {
            continue;
        }
        if let Ok(owner) = std::fs::read_to_string(from.join(OWNER_FILE)) {
            if owner.trim() != uid_text {
                continue;
            }
        }
        let target = dest.join(&name);
        if target.exists() {
            continue;
        }
        if std::fs::rename(&from, &target).is_ok() {
            stamp_owner(&target, uid);
        }
    }
}

/// Clone root scoped to the currently logged-in ZaloMulti UID (`<root>/accounts/<uid>`).
#[must_use]
pub fn account_clone_root() -> Option<PathBuf> {
    let uid = session_uid()?;
    let base = profile_root();
    let dest = base.join("accounts").join(uid.to_string());
    migrate_legacy_into(&base, &dest, uid);
    Some(dest)
}

/// Directory path for clone `name` under the signed-in account.
///
/// # Errors
///
/// Returns an error when logged out or when `name` is invalid.
pub fn profile_dir(name: &str) -> Result<PathBuf, String> {
    let root = account_clone_root().ok_or_else(|| NOT_LOGGED_IN.to_owned())?;
    contained_join(&root, name)
}

/// List valid clone names inside `dir`, sorted by UTF-8 byte order (`String::cmp`).
#[must_use]
pub fn list_profile_names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if canonical_profile_name(&name).ok().as_deref() == Some(name.as_str()) {
            names.push(name);
        }
    }
    names.sort();
    names
}

/// List clone names for the currently signed-in ZaloMulti account.
#[must_use]
pub fn list_profile_names() -> Vec<String> {
    match account_clone_root() {
        Some(dir) => list_profile_names_in(&dir),
        None => Vec::new(),
    }
}

/// Check whether `name` can be added given existing `names` and quota `max`.
///
/// # Errors
///
/// Returns duplicate or quota error.
pub fn decide_add(names: &[String], name: &str, max: usize) -> Result<(), String> {
    let lower = name.to_lowercase();
    if names.iter().any(|n| n.to_lowercase() == lower) {
        return Err("Tên tài khoản đã tồn tại!".into());
    }
    if names.len() >= max {
        return Err(limit_error(max));
    }
    Ok(())
}

/// Check whether `name` can be launched given existing `names` and quota `max`.
///
/// # Errors
///
/// Returns quota error if the profile exceeds `max`.
pub fn decide_launch(names: &[String], name: &str, max: usize) -> Result<(), String> {
    if let Some(idx) = names.iter().position(|n| n == name) {
        if idx < max {
            return Ok(());
        }
        return Err(limit_error(max));
    }
    if names.len() < max {
        return Ok(());
    }
    Err(limit_error(max))
}

/// Validate name, login status, and quota before adding a clone.
///
/// # Errors
///
/// Returns validation, auth, or quota error.
pub fn ensure_can_add(name: &str) -> Result<String, String> {
    let clean = canonical_profile_name(name)?;
    require_logged_in().map_err(|e| e.to_string())?;
    decide_add(&list_profile_names(), &clean, effective_max())?;
    Ok(clean)
}

/// Validate name, login status, and quota before launching a clone.
///
/// # Errors
///
/// Returns validation, auth, or quota error.
pub fn ensure_can_launch(name: &str) -> Result<String, String> {
    let clean = canonical_profile_name(name)?;
    require_logged_in().map_err(|e| e.to_string())?;
    decide_launch(&list_profile_names(), &clean, effective_max())?;
    Ok(clean)
}

/// Read optional `phone.txt` saved inside a clone directory.
#[must_use]
pub fn read_phone(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("phone.txt"))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// Peek the signed-in Zalo display name from `<dir>/xdg-cache/zalo-rust/gpui-profiles.json` (`map.me.name`).
#[must_use]
pub fn read_clone_zalo_name(dir: &Path) -> Option<String> {
    let path = dir
        .join("xdg-cache")
        .join("zalo-rust")
        .join("gpui-profiles.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    val.get("map")?
        .get("me")?
        .get("name")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Create a new clone directory under the signed-in ZaloMulti account.
///
/// # Errors
///
/// Returns validation, auth, quota, or I/O error.
pub fn add_profile(name: &str, phone: &str) -> Result<String, String> {
    let clean = ensure_can_add(name)?;
    let dir = profile_dir(&clean)?;
    if dir.exists() {
        return Err("Tên tài khoản đã tồn tại!".into());
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("Không tạo được thư mục clone: {e}"))?;
    if let Some(uid) = session_uid() {
        stamp_owner(&dir, uid);
    }
    let trimmed_phone = phone.trim();
    if !trimmed_phone.is_empty() {
        let _ = std::fs::write(dir.join("phone.txt"), trimmed_phone);
    }
    Ok(clean)
}

/// Rename a stopped clone profile.
///
/// # Errors
///
/// Returns an error if running, missing, invalid, or colliding.
pub fn rename_profile(old_name: &str, new_name: &str) -> Result<String, String> {
    let old_clean = canonical_profile_name(old_name)?;
    let new_clean = canonical_profile_name(new_name)?;
    if old_clean == new_clean {
        return Ok(new_clean);
    }
    let old_path = profile_dir(&old_clean)?;
    let new_path = profile_dir(&new_clean)?;
    if !old_path.exists() {
        return Err("Không tìm thấy tài khoản cũ!".into());
    }
    if crate::launcher::is_running(&old_clean, &old_path) {
        return Err("Hãy dừng clone trước khi đổi tên.".into());
    }
    if new_path.exists() {
        return Err("Tên tài khoản mới đã tồn tại!".into());
    }
    std::fs::rename(&old_path, &new_path).map_err(|e| format!("Không đổi tên được: {e}"))?;
    Ok(new_clean)
}

/// Update `phone.txt` note for a clone.
///
/// # Errors
///
/// Returns an error when the clone does not exist.
pub fn save_profile_phone(name: &str, phone: &str) -> Result<(), String> {
    let dir = profile_dir(name)?;
    if !dir.exists() {
        return Err("Không tìm thấy tài khoản!".into());
    }
    std::fs::write(dir.join("phone.txt"), phone.trim())
        .map_err(|e| format!("Không lưu được ghi chú: {e}"))
}

/// Delete a stopped clone profile directory permanently.
///
/// # Errors
///
/// Returns an error if running, missing, or deletion fails.
pub fn delete_profile_dir(name: &str) -> Result<(), String> {
    let clean = canonical_profile_name(name)?;
    let dir = profile_dir(&clean)?;
    if !dir.exists() {
        return Err("Không tìm thấy tài khoản để xóa!".into());
    }
    if crate::launcher::is_running(&clean, &dir) {
        return Err("Hãy dừng clone trước khi xóa.".into());
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("Không xóa được thư mục clone: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_profile_names_like_zalomulti() {
        assert_eq!(canonical_profile_name("  Tài khoản 1 ").unwrap(), "Tài khoản 1");
        let long = "x".repeat(65);
        for bad in [
            "",
            "..",
            "../x",
            "..\\Windows",
            "a/b",
            "a\\b",
            "a\"b",
            "a'b",
            "ZaloWeb",
            "accounts",
            "CON",
            ".hidden",
            "a\tb",
            long.as_str(),
        ] {
            assert!(canonical_profile_name(bad).is_err(), "expected error for {bad:?}");
        }
        assert_eq!(
            contained_join(Path::new("/tmp/root"), "Tài khoản 1").unwrap(),
            PathBuf::from("/tmp/root/Tài khoản 1")
        );
        assert!(contained_join(Path::new("/tmp/root"), "../etc").is_err());
    }

    #[test]
    fn quota_blocks_extra_profiles() {
        let names: Vec<String> = ["A", "B", "C", "D"].into_iter().map(str::to_owned).collect();
        assert!(decide_add(&names, "E", 4).unwrap_err().contains("tối đa 4"));
        assert!(decide_launch(&names, "D", 4).is_ok());
        assert!(decide_launch(&names, "E", 4).unwrap_err().contains("tối đa 4"));
        let mut five = names.clone();
        five.push("E".into());
        assert!(decide_launch(&five, "E", 4).unwrap_err().contains("tối đa 4"));
        assert!(decide_launch(&five, "E", 5).is_ok());
        assert!(decide_add(&["abc".into()], "ABC", 4)
            .unwrap_err()
            .contains("đã tồn tại"));
    }

    #[test]
    fn migrates_legacy_profiles_only_for_matching_or_unowned_uid() {
        let tmp = std::env::temp_dir().join(format!("zalo-multi-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("web")).unwrap();
        std::fs::write(tmp.join("web").join("phone.txt"), "033").unwrap();
        std::fs::create_dir_all(tmp.join("kept")).unwrap();
        std::fs::write(tmp.join("kept").join(OWNER_FILE), "99").unwrap();
        std::fs::create_dir_all(tmp.join("ZaloWeb")).unwrap();

        let dest39 = tmp.join("accounts").join("39");
        migrate_legacy_into(&tmp, &dest39, 39);
        assert_eq!(
            std::fs::read_to_string(dest39.join("web").join(OWNER_FILE)).unwrap(),
            "39"
        );
        assert_eq!(
            std::fs::read_to_string(dest39.join("web").join("phone.txt")).unwrap(),
            "033"
        );
        assert!(!dest39.join("kept").exists());
        assert!(tmp.join("kept").exists());
        assert!(tmp.join("ZaloWeb").exists());
        assert_eq!(list_profile_names_in(&dest39), vec!["web".to_owned()]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
