//! Local video probe for the composer send path.
//!
//! Duration and size come from `ffprobe`. A JPEG poster comes from `ffmpeg`.
//! Both tools are optional: a missing tool leaves duration at 1 ms and omits
//! the poster, which is what Zalo PC sends as `duration || 1`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Still metadata plus an optional poster file.
#[derive(Clone, Debug, Default)]
pub struct ProbedVideo {
    /// Length in milliseconds. `1` when `ffprobe` cannot read it.
    pub duration_ms: u64,
    /// Pixel width. `0` when unknown.
    pub width: u32,
    /// Pixel height. `0` when unknown.
    pub height: u32,
    /// JPEG written beside the cache. `None` when `ffmpeg` is missing.
    pub thumb: Option<PathBuf>,
}

/// True for extensions the file picker should send as `chat.video.msg`.
#[must_use]
pub fn is_video_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("mp4" | "mov" | "m4v" | "mkv" | "webm" | "avi" | "3gp")
    )
}

/// Header probe. Does not decode the whole file.
#[must_use]
pub fn inspect(path: &Path) -> ProbedVideo {
    let (duration_ms, width, height) = probe_header(path);
    let thumb = poster(path);
    ProbedVideo {
        duration_ms: if duration_ms == 0 { 1 } else { duration_ms },
        width,
        height,
        thumb,
    }
}

fn probe_header(path: &Path) -> (u64, u32, u32) {
    let Ok(out) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=width,height,duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
    else {
        return (0, 0, 0);
    };
    if !out.status.success() {
        return (0, 0, 0);
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return (0, 0, 0);
    };
    let stream = value
        .get("streams")
        .and_then(|v| v.as_array())
        .and_then(|list| list.iter().find(|s| s.get("width").is_some()))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let width = json_u32(stream.get("width"));
    let height = json_u32(stream.get("height"));
    let seconds = json_f64(stream.get("duration"))
        .or_else(|| json_f64(value.get("format").and_then(|f| f.get("duration"))))
        .unwrap_or(0.0);
    let duration_ms = if seconds > 0.0 {
        (seconds * 1000.0).round().clamp(1.0, u64::MAX as f64) as u64
    } else {
        0
    };
    (duration_ms, width, height)
}

fn json_u32(value: Option<&serde_json::Value>) -> u32 {
    match value {
        Some(serde_json::Value::Number(n)) => {
            n.as_u64().and_then(|v| u32::try_from(v).ok()).unwrap_or(0)
        }
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn json_f64(value: Option<&serde_json::Value>) -> Option<f64> {
    match value? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn poster(path: &Path) -> Option<PathBuf> {
    let dir = crate::media::media_root()
        .map(|root| root.join("photos"))
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).ok()?;
    let name = format!(
        "video-thumb-{}",
        crate::media::url_key(&path.display().to_string())
    );
    let dest = dir.join(format!("{name}.jpg"));
    let ok = Command::new("ffmpeg")
        .args(["-y", "-ss", "0.1", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", "scale=480:-1", "-q:v", "4"])
        .arg(&dest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .is_some_and(|s| s.success());
    (ok && dest.is_file() && std::fs::metadata(&dest).is_ok_and(|m| m.len() > 0)).then_some(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_extensions() {
        assert!(is_video_path(Path::new("/tmp/a.MP4")));
        assert!(is_video_path(Path::new("/tmp/a.webm")));
        assert!(!is_video_path(Path::new("/tmp/a.pdf")));
        assert!(!is_video_path(Path::new("/tmp/a")));
    }
}
