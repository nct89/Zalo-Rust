//! `NSOpenPanel` via `osascript`, and `screencapture` for a region grab.

use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::Command;

use crate::error::ShellError;

/// Glob patterns offered by the image picker (kept for the settings tests).
pub const IMAGE_PATTERNS: &[&str] = &[
    "*.png", "*.jpg", "*.jpeg", "*.webp", "*.gif", "*.bmp", "*.PNG", "*.JPG", "*.JPEG",
];

/// Full-desktop capture tools. Region flags are intentionally absent.
pub const SCREENSHOT_TOOLS: &[(&str, &[&str])] = &[("screencapture", &["-x", "{output}"])];

/// Portal-shaped constant kept so call sites share a name with the Linux shell.
pub const PORTAL_FILECHOOSER_INTERFACE: &str = "macos.NSOpenPanel";

/// `zenity --file-filter` text. macOS uses a UTI filter; the string stays for tests.
#[must_use]
pub fn zenity_filter(label: &str) -> String {
    format!("{label} | {}", IMAGE_PATTERNS.join(" "))
}

/// First absolute path in dialog stdout.
#[must_use]
pub fn parse_dialog_output(stdout: &str) -> Option<PathBuf> {
    let line = stdout.lines().next()?.trim();
    let path = PathBuf::from(line);
    path.is_absolute().then_some(path)
}

/// Absolute paths separated by newlines.
#[must_use]
pub fn parse_dialog_paths(stdout: &str) -> Vec<PathBuf> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .collect()
}

/// Image picker. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when `osascript` cannot run.
pub fn pick_image(title: &str) -> Result<Option<PathBuf>, ShellError> {
    pick_files(title, true, false).map(|mut paths| paths.pop())
}

/// Image picker that allows several files.
///
/// # Errors
///
/// Returns [`ShellError`] when `osascript` cannot run.
pub fn pick_images(title: &str) -> Result<Vec<PathBuf>, ShellError> {
    pick_files(title, true, true)
}

/// Any-file picker. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when `osascript` cannot run.
pub fn pick_file(title: &str) -> Result<Option<PathBuf>, ShellError> {
    pick_files(title, false, false).map(|mut paths| paths.pop())
}

/// Folder picker. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when `osascript` cannot run.
pub fn pick_folder(title: &str) -> Result<Option<PathBuf>, ShellError> {
    if cfg!(test) {
        return Ok(None);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = title;
        return Err(ShellError::Bus(
            "NSOpenPanel is only available on macOS".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            r#"POSIX path of (choose folder with prompt "{}")"#,
            applescript_string(title)
        );
        match osascript(&script) {
            Ok(stdout) => Ok(parse_dialog_output(&stdout)),
            Err(err) if user_cancelled(&err) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// Interactive region screenshot (`screencapture -i`). Empty when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when `screencapture` cannot start.
pub fn capture_interactive() -> Result<Vec<u8>, ShellError> {
    if cfg!(test) {
        return Ok(Vec::new());
    }
    #[cfg(not(target_os = "macos"))]
    {
        return Err(ShellError::Bus(
            "screencapture is only available on macOS".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let path = std::env::temp_dir().join(format!(
            "zalo-shot-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let status = Command::new("screencapture")
            .args(["-i", "-x"])
            .arg(&path)
            .status()
            .map_err(|err| ShellError::Bus(err.to_string()))?;
        if !status.success() || !path.is_file() {
            let _ = std::fs::remove_file(&path);
            return Err(ShellError::Bus("screencapture cancelled status 1".into()));
        }
        let bytes = std::fs::read(&path).map_err(|err| ShellError::Io(err.to_string()))?;
        let _ = std::fs::remove_file(&path);
        Ok(bytes)
    }
}

/// Full-screen PNG via `screencapture -x`.
///
/// # Errors
///
/// Returns [`ShellError`] when the tool fails.
pub fn capture_fullscreen() -> Result<Vec<u8>, ShellError> {
    capture_with_args(&["-x"])
}

/// Alias of [`capture_fullscreen`].
///
/// # Errors
///
/// Returns [`ShellError`] when the tool fails.
pub fn capture_screenshot() -> Result<Vec<u8>, ShellError> {
    capture_fullscreen()
}

fn capture_with_args(args: &[&str]) -> Result<Vec<u8>, ShellError> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        return Err(ShellError::Bus(
            "screencapture is only available on macOS".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let path = std::env::temp_dir().join(format!(
            "zalo-full-{}.png",
            std::process::id()
        ));
        let status = Command::new("screencapture")
            .args(args)
            .arg(&path)
            .status()
            .map_err(|err| ShellError::Bus(err.to_string()))?;
        if !status.success() {
            return Err(ShellError::Bus(format!("screencapture status {status}")));
        }
        let bytes = std::fs::read(&path).map_err(|err| ShellError::Io(err.to_string()))?;
        let _ = std::fs::remove_file(&path);
        Ok(bytes)
    }
}

fn pick_files(title: &str, images: bool, multiple: bool) -> Result<Vec<PathBuf>, ShellError> {
    if cfg!(test) {
        return Ok(Vec::new());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, images, multiple);
        return Err(ShellError::Bus(
            "NSOpenPanel is only available on macOS".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let title = applescript_string(title);
        let of_type = if images {
            r#" of type {"public.image"}"#
        } else {
            ""
        };
        let script = if multiple {
            format!(
                r#"set picked to choose file with prompt "{title}" with multiple selections allowed{of_type}
set output to ""
repeat with f in picked
set output to output & POSIX path of f & linefeed
end repeat
output"#
            )
        } else {
            format!(r#"POSIX path of (choose file with prompt "{title}"{of_type})"#)
        };
        match osascript(&script) {
            Ok(stdout) => Ok(parse_dialog_paths(&stdout)),
            Err(err) if user_cancelled(&err) => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(target_os = "macos")]
fn osascript(script: &str) -> Result<String, ShellError> {
    let out = Command::new("osascript")
        .args(["-e", script])
        .output()
        .map_err(|err| ShellError::Bus(err.to_string()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(ShellError::Bus(if stderr.is_empty() {
            format!("osascript status {}", out.status)
        } else {
            stderr
        }));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(target_os = "macos")]
fn applescript_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
fn user_cancelled(err: &ShellError) -> bool {
    let ShellError::Bus(msg) = err else {
        return false;
    };
    msg.contains("-128") || msg.to_ascii_lowercase().contains("cancel")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_parser_rejects_relative_paths() {
        assert!(parse_dialog_output("relative.png").is_none());
        assert_eq!(
            parse_dialog_output("/tmp/a.png\n"),
            Some(PathBuf::from("/tmp/a.png"))
        );
        assert_eq!(
            parse_dialog_paths("/tmp/a.png\nrelative\n/tmp/b.jpg\n"),
            vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.jpg")]
        );
        assert!(zenity_filter("Ảnh").contains("*.png"));
        for (_bin, args) in SCREENSHOT_TOOLS {
            assert!(!args.iter().any(|flag| *flag == "-r" || *flag == "-s" || *flag == "gui"));
        }
    }
}
