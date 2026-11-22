//! In-app GitHub Release update checker and desktop notification helper.
//!
//! Compares the compiled [`crate::APP_VERSION`] against the latest published
//! GitHub Release (`https://api.github.com/repos/nct89/Zalo-Rust/releases/latest`)
//! using [`zalo_core::version::Version`]. Runs completely asynchronously off the
//! UI thread so cold startup is never blocked.

use serde_json::Value;
use zalo_core::version::Version;

/// GitHub Releases API endpoint for the latest published release.
pub const LATEST_RELEASE_API_URL: &str =
    "https://api.github.com/repos/nct89/Zalo-Rust/releases/latest";

/// Periodic background check interval (6 hours).
pub const UPDATE_POLL_INTERVAL_SECS: u64 = 6 * 3600;

/// Metadata for a newer release available on GitHub Releases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateInfo {
    /// Display tag, e.g. `v0.2.3`.
    pub tag: String,
    /// Clean numeric version, e.g. `0.2.3`.
    pub version: String,
    /// GitHub release page URL.
    pub release_url: String,
    /// Preferred OS package download URL (or `release_url` fallback).
    pub download_url: String,
    /// Short single-line summary extracted from release notes.
    pub summary: String,
}

/// Result of one update check round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateCheckResult {
    /// A newer stable release was found.
    Available(UpdateInfo),
    /// Current build is up to date (or newer than latest release).
    UpToDate,
    /// Server returned HTTP 304 Not Modified (ETag matched).
    NotModified,
    /// Network or JSON error.
    Failed(String),
}

/// Strips a leading `v`/`V` and any `-prerelease` / `+build` suffix for numeric comparison.
fn normalize_numeric_version(raw: &str) -> &str {
    let stripped = raw.trim().trim_start_matches(['v', 'V']);
    stripped
        .split(['-', '+'])
        .next()
        .unwrap_or(stripped)
        .trim()
}

/// Returns `true` when `candidate` is strictly newer than `current` numerically.
#[must_use]
pub fn is_newer_version(current: &str, candidate: &str) -> bool {
    let Ok(cur) = normalize_numeric_version(current).parse::<Version>() else {
        return false;
    };
    let Ok(cand) = normalize_numeric_version(candidate).parse::<Version>() else {
        return false;
    };
    cand > cur
}

/// Picks the most appropriate release asset URL for the given `os` (`linux`, `macos`, `windows`).
#[must_use]
pub fn pick_platform_asset(assets: &[Value], os: &str) -> Option<String> {
    let preferred: &[&str] = match os {
        "linux" => &[".deb", "-linux-x86_64.tar.gz", ".tar.gz"],
        "macos" => &["macos", ".dmg", ".zip"],
        "windows" => &["-setup.exe", "windows", ".zip"],
        _ => &[],
    };
    for needle in preferred {
        for asset in assets {
            let name = asset
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let url = asset
                .get("browser_download_url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if !url.is_empty() && name.contains(needle) {
                return Some(url.to_owned());
            }
        }
    }
    None
}

/// Parses a GitHub Releases `/latest` JSON payload and returns [`UpdateInfo`] if
/// it is a non-draft, non-prerelease version newer than `current_ver`.
#[must_use]
pub fn parse_release_json(current_ver: &str, body: &Value, os: &str) -> Option<UpdateInfo> {
    if body.get("draft").and_then(Value::as_bool).unwrap_or(false)
        || body
            .get("prerelease")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return None;
    }
    let raw_tag = body.get("tag_name").and_then(Value::as_str)?.trim();
    if raw_tag.is_empty() || !is_newer_version(current_ver, raw_tag) {
        return None;
    }
    let version = normalize_numeric_version(raw_tag).to_owned();
    let tag = if raw_tag.starts_with(['v', 'V']) {
        raw_tag.to_owned()
    } else {
        format!("v{version}")
    };
    let release_url = body
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}/releases/tag/{tag}", crate::APP_REPO_URL));
    let assets = body
        .get("assets")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let download_url = pick_platform_asset(assets, os).unwrap_or_else(|| release_url.clone());
    let summary = body
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("")
        .chars()
        .take(160)
        .collect();
    Some(UpdateInfo {
        tag,
        version,
        release_url,
        download_url,
        summary,
    })
}

/// Fetches the latest GitHub Release asynchronously and compares it with `current_ver`.
///
/// Honors `ZALO_FORCE_LATEST_VERSION` (e.g. `v9.9.9`) for manual UI verification
/// and `ZALO_UPDATE_API_URL` for custom mirrors.
pub async fn check_latest_release(
    current_ver: &str,
    etag: Option<&str>,
) -> (UpdateCheckResult, Option<String>) {
    if let Ok(forced) = std::env::var("ZALO_FORCE_LATEST_VERSION") {
        let trimmed = forced.trim();
        if !trimmed.is_empty() && is_newer_version(current_ver, trimmed) {
            let ver = normalize_numeric_version(trimmed).to_owned();
            let tag = format!("v{ver}");
            let release_url = format!("{}/releases/latest", crate::APP_REPO_URL);
            return (
                UpdateCheckResult::Available(UpdateInfo {
                    tag,
                    version: ver,
                    download_url: release_url.clone(),
                    release_url,
                    summary: "Phiên bản cập nhật thử nghiệm".to_owned(),
                }),
                None,
            );
        }
    }

    let url = std::env::var("ZALO_UPDATE_API_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| LATEST_RELEASE_API_URL.to_owned());

    let client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(format!("Zalo-Rust/{}", crate::APP_VERSION))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (UpdateCheckResult::Failed(e.to_string()), None),
    };

    let mut req = client
        .get(&url)
        .header("Accept", "application/vnd.github+json");
    if let Some(tag) = etag.filter(|s| !s.trim().is_empty()) {
        req = req.header("If-None-Match", tag);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return (UpdateCheckResult::Failed(e.to_string()), None),
    };

    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return (UpdateCheckResult::NotModified, etag.map(str::to_owned));
    }
    if !resp.status().is_success() {
        return (
            UpdateCheckResult::Failed(format!("HTTP {}", resp.status().as_u16())),
            None,
        );
    }

    let next_etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let json: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return (UpdateCheckResult::Failed(e.to_string()), next_etag),
    };

    match parse_release_json(current_ver, &json, std::env::consts::OS) {
        Some(info) => (UpdateCheckResult::Available(info), next_etag),
        None => (UpdateCheckResult::UpToDate, next_etag),
    }
}

/// Sends a one-shot OS desktop notification informing the user of a new release.
pub fn notify_update_available(info: &UpdateInfo) {
    if cfg!(test) {
        return;
    }
    let notice = zalo_shell::DesktopNotice {
        app_name: "Zalo".to_owned(),
        summary: format!("Zalo Rust có phiên bản mới {}", info.tag),
        body: format!(
            "Phiên bản hiện tại {}. Mở Thông tin phiên bản để tải bản cập nhật.",
            crate::APP_VERSION_TAG
        ),
        expire_ms: 8000,
        icon_path: None,
    };
    let _ = std::thread::Builder::new()
        .name("zalo-update-notify".into())
        .spawn(move || {
            let _ = zalo_shell::notify_desktop(&notice);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compares_semver_tags_numerically() {
        assert!(is_newer_version("0.2.2", "v0.2.3"));
        assert!(is_newer_version("0.2.9", "v0.2.10"));
        assert!(is_newer_version("v0.2.2", "0.3.0"));
        assert!(!is_newer_version("0.2.2", "v0.2.2"));
        assert!(!is_newer_version("0.2.3", "v0.2.2"));
    }

    #[test]
    fn parses_github_release_and_picks_os_asset() {
        let payload = json!({
            "tag_name": "v0.2.5",
            "html_url": "https://github.com/nct89/Zalo-Rust/releases/tag/v0.2.5",
            "draft": false,
            "prerelease": false,
            "body": "## Highlights\nSync 2 WASM fix and auto updater.",
            "assets": [
                {
                    "name": "zalo-linux_0.2.5_amd64.deb",
                    "browser_download_url": "https://example.invalid/zalo.deb"
                },
                {
                    "name": "Zalo-0.2.5-macos-aarch64.zip",
                    "browser_download_url": "https://example.invalid/zalo-mac.zip"
                },
                {
                    "name": "zalo-windows-0.2.5-setup.exe",
                    "browser_download_url": "https://example.invalid/zalo-setup.exe"
                }
            ]
        });

        let linux = parse_release_json("0.2.2", &payload, "linux").expect("linux update");
        assert_eq!(linux.tag, "v0.2.5");
        assert_eq!(linux.download_url, "https://example.invalid/zalo.deb");
        assert_eq!(linux.summary, "Sync 2 WASM fix and auto updater.");

        let mac = parse_release_json("0.2.2", &payload, "macos").expect("mac update");
        assert_eq!(mac.download_url, "https://example.invalid/zalo-mac.zip");

        let win = parse_release_json("0.2.2", &payload, "windows").expect("win update");
        assert_eq!(win.download_url, "https://example.invalid/zalo-setup.exe");
    }

    #[test]
    fn ignores_draft_prerelease_and_older_versions() {
        let draft = json!({"tag_name": "v0.9.0", "draft": true, "prerelease": false});
        assert!(parse_release_json("0.2.2", &draft, "linux").is_none());

        let pre = json!({"tag_name": "v0.9.0-rc1", "draft": false, "prerelease": true});
        assert!(parse_release_json("0.2.2", &pre, "linux").is_none());

        let same = json!({"tag_name": "v0.2.2", "draft": false, "prerelease": false});
        assert!(parse_release_json("0.2.2", &same, "linux").is_none());
    }
}
