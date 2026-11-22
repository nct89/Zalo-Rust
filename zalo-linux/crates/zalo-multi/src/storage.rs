//! Profile storage root validation & relocation (`storage.js` parity).

use std::path::{Path, PathBuf};

use crate::paths::{app_data_dir, profile_root};

const BANNED: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/proc", "/run", "/sbin", "/sys", "/usr",
    "/var",
];

/// Result returned after changing the custom clone storage root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootMoveOutcome {
    /// Newly active clone root path.
    pub path: PathBuf,
}

/// Validate a candidate clone root directory path.
///
/// # Errors
///
/// Rejects relative paths, control characters, `/`, and system directories.
pub fn validate_root(value: &str) -> Result<PathBuf, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().any(|c| c.is_control()) {
        return Err("Thư mục lưu tài khoản không hợp lệ.".into());
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err("Thư mục lưu tài khoản không hợp lệ.".into());
    }
    let normalized = normalize_path(&path);
    if normalized == Path::new("/") {
        return Err("Không dùng thư mục gốc của ổ đĩa.".into());
    }
    let s = normalized.to_string_lossy();
    for banned in BANNED {
        if s == *banned || s.starts_with(&format!("{banned}/")) {
            return Err("Thư mục này không được dùng để lưu tài khoản.".into());
        }
    }
    Ok(normalized)
}

fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(p) => out.push(p),
            Component::Prefix(_) => {}
        }
    }
    out
}

/// Verify that `dir` can be created and written to.
///
/// # Errors
///
/// Returns a Vietnamese message when creation or write probe fails.
pub fn ensure_writable(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|_| "Không tạo được thư mục này. Hãy chọn thư mục khác.".to_owned())?;
    let probe = dir.join(".zalomulti-write-test");
    std::fs::write(&probe, "ok")
        .map_err(|_| "Không có quyền ghi vào thư mục này. Hãy chọn thư mục khác.".to_owned())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Persist a new clone root path in `~/.config/ZaloMulti/custom_path.txt`.
///
/// # Errors
///
/// Fails if any clone is running, path is invalid/nested, or not writable.
pub fn set_profile_root(value: &str, any_running: bool) -> Result<RootMoveOutcome, String> {
    let new_root = validate_root(value)?;
    let old_root = normalize_path(&profile_root());
    if new_root == old_root {
        return Err("Đây đã là thư mục lưu hiện tại.".into());
    }
    if new_root.starts_with(&old_root) {
        return Err("Không chọn thư mục nằm bên trong thư mục lưu hiện tại.".into());
    }
    if any_running {
        return Err("Hãy dừng tất cả Zalo trước khi đổi thư mục lưu.".into());
    }
    ensure_writable(&new_root)?;
    std::fs::write(
        app_data_dir().join("custom_path.txt"),
        new_root.to_string_lossy().as_bytes(),
    )
    .map_err(|e| format!("Không lưu được cấu hình thư mục: {e}"))?;
    Ok(RootMoveOutcome { path: new_root })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_root_relative_and_system_dirs() {
        assert!(validate_root("").is_err());
        assert!(validate_root("relative/path").is_err());
        assert!(validate_root("/").is_err());
        assert!(validate_root("/etc/zalo").is_err());
        assert!(validate_root("/usr").is_err());
        assert_eq!(
            validate_root("/home/user/ZaloClones/").unwrap(),
            PathBuf::from("/home/user/ZaloClones")
        );
    }
}
