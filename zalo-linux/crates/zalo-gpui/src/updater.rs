//! Cross-platform in-app GitHub Release auto-updater (Linux, macOS, Windows).
//!
//! Responsibilities:
//! 1. Periodically check `https://api.github.com/repos/nct89/Zalo-Rust/releases/latest`
//!    asynchronously off the UI thread (with ETag caching).
//! 2. Detect the active OS, CPU architecture, and installation mode (`.deb` vs
//!    portable `.tar.gz` on Linux, `.app` bundle `.zip` on macOS, Inno Setup
//!    `-setup.exe` vs portable `.zip` on Windows) and select the matching asset
//!    plus its `SHA256SUMS*.txt` manifest.
//! 3. Stream-download the selected package with live byte progress, verify its
//!    SHA-256 digest when published, apply the update atomically (or hand off to
//!    a detached platform helper script), and relaunch the updated application.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use sha2::{Digest, Sha256};
use zalo_core::version::Version;

/// GitHub Releases API endpoint for the latest published release.
pub const LATEST_RELEASE_API_URL: &str =
    "https://api.github.com/repos/nct89/Zalo-Rust/releases/latest";

/// Periodic background check interval (6 hours).
pub const UPDATE_POLL_INTERVAL_SECS: u64 = 6 * 3600;

/// Kind of downloadable update artifact selected for this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    /// Debian/Ubuntu `.deb` system package (`/usr/bin/zalo-gpui`).
    LinuxDeb,
    /// Portable / user-local Linux `.tar.gz` archive (`~/.local/bin/zalo-gpui`).
    LinuxTarGz,
    /// macOS `Zalo.app` zip archive created with `ditto`.
    MacOsZip,
    /// Windows Inno Setup installer (`*-setup.exe`).
    WindowsSetupExe,
    /// Windows portable zip archive (`Zalo-*-windows-*.zip`).
    WindowsZip,
    /// Release page / external URL when no direct binary asset matches.
    External,
}

impl PackageKind {
    /// Short human-readable package format label for the update popup.
    #[must_use]
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::LinuxDeb => "Linux (.deb)",
            Self::LinuxTarGz => "Linux (.tar.gz)",
            Self::MacOsZip => "macOS (.app)",
            Self::WindowsSetupExe => "Windows (Setup)",
            Self::WindowsZip => "Windows (Portable)",
            Self::External => "GitHub Releases",
        }
    }

    /// Returns `true` when this package can be downloaded and installed in-app.
    #[must_use]
    pub const fn supports_in_app_install(self) -> bool {
        !matches!(self, Self::External)
    }
}

/// Detected installation layout of the currently running `zalo-gpui` binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallTarget {
    /// System-wide Debian/Ubuntu package (e.g. `/usr/bin/zalo-gpui`).
    LinuxDeb {
        /// Installed executable path.
        exe_path: PathBuf,
    },
    /// User-writable Linux install (`~/.local/bin/zalo-gpui` or portable tarball).
    LinuxPortable {
        /// Target executable path to replace atomically.
        exe_path: PathBuf,
        /// Target runtime asset directory (`zalo-rust/`).
        data_dir: PathBuf,
    },
    /// macOS `.app` bundle (e.g. `/Applications/Zalo.app` or `~/Applications/Zalo.app`).
    MacOsAppBundle {
        /// Path to `Zalo.app`.
        app_bundle: PathBuf,
    },
    /// Windows Inno Setup install (`unins000.exe` beside `zalo-gpui.exe`).
    WindowsInnoSetup {
        /// Installed `zalo-gpui.exe` path.
        exe_path: PathBuf,
    },
    /// Windows portable folder install (no `unins000.exe`).
    WindowsPortable {
        /// Running `zalo-gpui.exe` path.
        exe_path: PathBuf,
    },
}

/// Metadata for a newer release available on GitHub Releases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateInfo {
    /// Display tag, e.g. `v0.3.4`.
    pub tag: String,
    /// Clean numeric version, e.g. `0.3.4`.
    pub version: String,
    /// GitHub release page URL.
    pub release_url: String,
    /// Preferred OS package download URL (or `release_url` fallback).
    pub download_url: String,
    /// Filename of the selected release asset (empty when falling back to `release_url`).
    pub asset_name: String,
    /// Declared byte size of the selected asset, if provided by GitHub.
    pub asset_size: Option<u64>,
    /// Selected package type driving the in-app installer strategy.
    pub package_kind: PackageKind,
    /// Matching `SHA256SUMS*.txt` URL in the same release, if published.
    pub checksum_url: Option<String>,
    /// Short single-line summary extracted from release notes.
    pub summary: String,
    /// Multi-line release notes preview displayed in the update popup.
    pub notes: String,
}

impl UpdateInfo {
    /// Formatted package label including size when known, e.g. `Linux (.tar.gz · 24.8 MB)`.
    #[must_use]
    pub fn package_badge(&self) -> String {
        match self.asset_size {
            Some(bytes) if bytes > 0 => {
                format!("{} · {}", self.package_kind.short_label(), format_bytes(bytes))
            }
            _ => self.package_kind.short_label().to_owned(),
        }
    }
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

/// Live UI state of an in-app update download & install run.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum UpdateInstallState {
    /// No install currently running.
    #[default]
    Idle,
    /// Streaming package bytes from GitHub Releases.
    Downloading {
        /// Bytes downloaded so far.
        downloaded: u64,
        /// Total bytes from `Content-Length` or release metadata, if known.
        total: Option<u64>,
    },
    /// Verifying downloaded file SHA-256 against `SHA256SUMS*.txt`.
    Verifying,
    /// Extracting and applying the package for the current OS.
    Installing {
        /// Status line describing the active installation step.
        message: String,
    },
    /// Update applied or handoff helper launched; app is restarting.
    Restarting {
        /// Final status line shown right before exiting.
        message: String,
    },
    /// Download, verification, or installation failed.
    Failed(String),
}

impl UpdateInstallState {
    /// Returns `true` while an active download/install/restart is in flight.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Downloading { .. }
                | Self::Verifying
                | Self::Installing { .. }
                | Self::Restarting { .. }
        )
    }

    /// Progress fraction in `0.0..=1.0` for the progress bar.
    #[must_use]
    pub fn progress_fraction(&self) -> f32 {
        match self {
            Self::Idle | Self::Failed(_) => 0.0,
            Self::Downloading { downloaded, total } => match total {
                Some(t) if *t > 0 => ((*downloaded as f64 / *t as f64) as f32).clamp(0.02, 0.80),
                _ => 0.25,
            },
            Self::Verifying => 0.84,
            Self::Installing { .. } => 0.92,
            Self::Restarting { .. } => 1.0,
        }
    }
}

/// Selected release asset metadata returned by [`pick_platform_asset_for`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedAsset {
    /// Asset filename.
    pub name: String,
    /// Direct download URL.
    pub url: String,
    /// Asset size in bytes, if present in release JSON.
    pub size: Option<u64>,
    /// Detected package kind.
    pub kind: PackageKind,
    /// Matching checksum file URL in the same release, if any.
    pub checksum_url: Option<String>,
}

/// Outcome returned by [`download_and_apply_update`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Updated binary/helper was launched; caller should quit the current process.
    QuitToRestart,
    /// No in-app binary package existed; caller should open `release_url` in browser.
    OpenReleasePage,
}

/// Formats a byte count concisely (`12.4 MB`, `840 KB`, etc.).
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
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

/// Detects the installation layout for the currently running binary.
#[must_use]
pub fn detect_install_target() -> InstallTarget {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zalo-gpui"));
    detect_install_target_for(std::env::consts::OS, &exe)
}

/// Pure helper determining [`InstallTarget`] for a given `os` and `exe_path`.
#[must_use]
pub fn detect_install_target_for(os: &str, exe_path: &Path) -> InstallTarget {
    match os {
        "macos" => {
            if let Some(bundle) = macos_app_bundle_from_exe(exe_path) {
                InstallTarget::MacOsAppBundle { app_bundle: bundle }
            } else {
                let fallback = std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join("Applications/Zalo.app"))
                    .unwrap_or_else(|| PathBuf::from("/Applications/Zalo.app"));
                InstallTarget::MacOsAppBundle {
                    app_bundle: fallback,
                }
            }
        }
        "windows" => {
            let is_inno = exe_path
                .parent()
                .map(|dir| dir.join("unins000.exe").is_file())
                .unwrap_or(false);
            if is_inno {
                InstallTarget::WindowsInnoSetup {
                    exe_path: exe_path.to_path_buf(),
                }
            } else {
                InstallTarget::WindowsPortable {
                    exe_path: exe_path.to_path_buf(),
                }
            }
        }
        _ => {
            let system_path = exe_path.starts_with("/usr/bin")
                || exe_path.starts_with("/usr/local/bin")
                || exe_path.starts_with("/opt/");
            if system_path && is_debian_deb_installed(exe_path) {
                InstallTarget::LinuxDeb {
                    exe_path: exe_path.to_path_buf(),
                }
            } else {
                let data_dir = resolve_linux_data_dir(exe_path);
                InstallTarget::LinuxPortable {
                    exe_path: exe_path.to_path_buf(),
                    data_dir,
                }
            }
        }
    }
}

/// Returns `true` when the current Linux binary belongs to an installed Debian `.deb` package.
fn is_debian_deb_installed(exe_path: &Path) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    if Command::new("dpkg-query")
        .args(["-S"])
        .arg(exe_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    // Fallback heuristic if `/usr/bin/zalo-gpui` exists on a Debian-based system.
    exe_path.starts_with("/usr/bin") && Path::new("/usr/bin/dpkg").is_file()
}

/// Resolves the matching `zalo-rust` data dir for a Linux portable or user-local binary.
fn resolve_linux_data_dir(exe_path: &Path) -> PathBuf {
    // Portable layout: `<prefix>/bin/zalo-gpui` + `<prefix>/share/zalo-rust`.
    if let Some(bin_dir) = exe_path.parent() {
        if bin_dir.file_name().and_then(|n| n.to_str()) == Some("bin") {
            if let Some(prefix) = bin_dir.parent() {
                let sibling = prefix.join("share/zalo-rust");
                if sibling.is_dir() {
                    return sibling;
                }
            }
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("zalo-rust");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join(".local/share/zalo-rust");
    }
    PathBuf::from("/tmp/zalo-rust")
}

/// Extracts `.../Zalo.app` from `.../Zalo.app/Contents/MacOS/zalo-gpui`.
#[must_use]
pub fn macos_app_bundle_from_exe(exe_path: &Path) -> Option<PathBuf> {
    let macos_dir = exe_path.parent()?;
    if macos_dir.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name()?.to_str()? != "Contents" {
        return None;
    }
    let app_dir = contents_dir.parent()?;
    if app_dir.extension()?.to_str()? == "app" {
        Some(app_dir.to_path_buf())
    } else {
        None
    }
}

/// Whether the given [`InstallTarget`] prefers a system/setup installer over a portable archive.
#[must_use]
pub fn prefers_installer_package(target: &InstallTarget) -> bool {
    matches!(
        target,
        InstallTarget::LinuxDeb { .. } | InstallTarget::WindowsInnoSetup { .. }
    )
}

fn arch_needles(arch: &str) -> &'static [&'static str] {
    match arch {
        "aarch64" | "arm64" => &["aarch64", "arm64"],
        _ => &["x86_64", "amd64", "x64"],
    }
}

fn pick_checksum_url(assets: &[Value], os: &str) -> Option<String> {
    let candidates: &[&str] = match os {
        "windows" => &["sha256sums-windows.txt", "sha256sums.txt"],
        "macos" => &["sha256sums-macos.txt", "sha256sums.txt"],
        _ => &["sha256sums-linux.txt", "sha256sums.txt"],
    };
    for want in candidates {
        for asset in assets {
            let name = asset
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let url = asset
                .get("browser_download_url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if !url.is_empty() && name == *want {
                return Some(url.to_owned());
            }
        }
    }
    None
}

fn asset_matches_arch(lower_name: &str, arch: &str) -> bool {
    arch_needles(arch).iter().any(|n| lower_name.contains(n))
}

/// Picks the best release asset URL for `os` (`linux`, `macos`, `windows`) using default arch/mode.
#[must_use]
#[allow(dead_code)]
pub fn pick_platform_asset(assets: &[Value], os: &str) -> Option<String> {
    let prefer_installer = matches!(os, "linux" | "windows");
    pick_platform_asset_for(assets, os, std::env::consts::ARCH, prefer_installer).map(|a| a.url)
}

/// Picks the most appropriate release asset (plus checksum URL) for `os`, `arch`, and install mode.
#[must_use]
pub fn pick_platform_asset_for(
    assets: &[Value],
    os: &str,
    arch: &str,
    prefer_installer: bool,
) -> Option<SelectedAsset> {
    let checksum_url = pick_checksum_url(assets, os);

    // Ordered matching passes: `(ext_or_needle, require_arch, kind)`.
    let passes: Vec<(&str, bool, PackageKind)> = match os {
        "linux" if prefer_installer => vec![
            (".deb", true, PackageKind::LinuxDeb),
            (".deb", false, PackageKind::LinuxDeb),
            (".tar.gz", true, PackageKind::LinuxTarGz),
            (".tar.gz", false, PackageKind::LinuxTarGz),
        ],
        "linux" => vec![
            (".tar.gz", true, PackageKind::LinuxTarGz),
            (".tar.gz", false, PackageKind::LinuxTarGz),
            (".deb", true, PackageKind::LinuxDeb),
            (".deb", false, PackageKind::LinuxDeb),
        ],
        "macos" => vec![
            (".zip", true, PackageKind::MacOsZip),
            (".zip", false, PackageKind::MacOsZip),
        ],
        "windows" if prefer_installer => vec![
            ("-setup.exe", true, PackageKind::WindowsSetupExe),
            ("-setup.exe", false, PackageKind::WindowsSetupExe),
            (".zip", true, PackageKind::WindowsZip),
            (".zip", false, PackageKind::WindowsZip),
        ],
        "windows" => vec![
            (".zip", true, PackageKind::WindowsZip),
            (".zip", false, PackageKind::WindowsZip),
            ("-setup.exe", true, PackageKind::WindowsSetupExe),
            ("-setup.exe", false, PackageKind::WindowsSetupExe),
        ],
        _ => Vec::new(),
    };

    for (suffix, require_arch, kind) in passes {
        for asset in assets {
            let raw_name = asset
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let lower = raw_name.to_ascii_lowercase();
            let url = asset
                .get("browser_download_url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if raw_name.is_empty() || url.is_empty() || !lower.ends_with(suffix) {
                continue;
            }
            // Guard `.zip` against matching another OS's zip asset.
            if suffix == ".zip" {
                if os == "macos" && !lower.contains("macos") && !lower.contains("darwin") {
                    continue;
                }
                if os == "windows"
                    && !lower.contains("windows")
                    && (!lower.contains("win") || lower.contains("darwin") || lower.contains("macos"))
                {
                    continue;
                }
            }
            if suffix == ".tar.gz" && !lower.contains("linux") {
                continue;
            }
            if require_arch && !asset_matches_arch(&lower, arch) {
                continue;
            }
            let size = asset.get("size").and_then(Value::as_u64);
            return Some(SelectedAsset {
                name: raw_name.to_owned(),
                url: url.to_owned(),
                size,
                kind,
                checksum_url,
            });
        }
    }
    None
}

/// Extracts clean multi-line release notes (up to 10 lines / 700 chars) from Markdown body.
#[must_use]
pub fn extract_release_notes(body: &str) -> String {
    let mut lines = Vec::new();
    let mut total_chars = 0usize;
    for raw in body.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with("<!--") {
            continue;
        }
        let cleaned = trimmed
            .trim_start_matches('#')
            .trim()
            .replace("**", "")
            .replace('`', "");
        if cleaned.is_empty() {
            continue;
        }
        // Skip boilerplate GitHub Full Changelog links.
        if cleaned.to_ascii_lowercase().starts_with("full changelog:") {
            continue;
        }
        let line: String = cleaned.chars().take(120).collect();
        total_chars += line.len() + 1;
        lines.push(line);
        if lines.len() >= 10 || total_chars >= 700 {
            break;
        }
    }
    lines.join("\n")
}

/// Parses a GitHub Releases `/latest` JSON payload and returns [`UpdateInfo`] if
/// it is a non-draft, non-prerelease version newer than `current_ver`.
#[cfg(test)]
#[must_use]
pub fn parse_release_json(current_ver: &str, body: &Value, os: &str) -> Option<UpdateInfo> {
    let prefer_installer = matches!(os, "linux" | "windows");
    parse_release_json_for(
        current_ver,
        body,
        os,
        std::env::consts::ARCH,
        prefer_installer,
    )
}

/// Parses a GitHub Releases JSON payload for a specific `os`, `arch`, and install preference.
#[must_use]
pub fn parse_release_json_for(
    current_ver: &str,
    body: &Value,
    os: &str,
    arch: &str,
    prefer_installer: bool,
) -> Option<UpdateInfo> {
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

    let (download_url, asset_name, asset_size, package_kind, checksum_url) =
        match pick_platform_asset_for(assets, os, arch, prefer_installer) {
            Some(sel) => (sel.url, sel.name, sel.size, sel.kind, sel.checksum_url),
            None => (
                release_url.clone(),
                String::new(),
                None,
                PackageKind::External,
                None,
            ),
        };

    let raw_body = body.get("body").and_then(Value::as_str).unwrap_or("");
    let summary = raw_body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("")
        .chars()
        .take(160)
        .collect();
    let notes = extract_release_notes(raw_body);

    Some(UpdateInfo {
        tag,
        version,
        release_url,
        download_url,
        asset_name,
        asset_size,
        package_kind,
        checksum_url,
        summary,
        notes,
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
            let target = detect_install_target();
            let (asset_name, package_kind) = match target {
                InstallTarget::LinuxDeb { .. } => (
                    format!("zalo-linux_{ver}_amd64.deb"),
                    PackageKind::LinuxDeb,
                ),
                InstallTarget::LinuxPortable { .. } => (
                    format!("zalo-linux-{ver}-linux-{}.tar.gz", std::env::consts::ARCH),
                    PackageKind::LinuxTarGz,
                ),
                InstallTarget::MacOsAppBundle { .. } => (
                    format!("Zalo-{ver}-macos-{}-apple-darwin.zip", std::env::consts::ARCH),
                    PackageKind::MacOsZip,
                ),
                InstallTarget::WindowsInnoSetup { .. } => (
                    format!("Zalo-{ver}-windows-{}-setup.exe", std::env::consts::ARCH),
                    PackageKind::WindowsSetupExe,
                ),
                InstallTarget::WindowsPortable { .. } => (
                    format!("Zalo-{ver}-windows-{}.zip", std::env::consts::ARCH),
                    PackageKind::WindowsZip,
                ),
            };
            let download_url = std::env::var("ZALO_FORCE_UPDATE_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| release_url.clone());
            let effective_kind = if download_url == release_url {
                PackageKind::External
            } else {
                package_kind
            };
            return (
                UpdateCheckResult::Available(UpdateInfo {
                    tag,
                    version: ver,
                    download_url,
                    release_url,
                    asset_name,
                    asset_size: Some(26_400_000),
                    package_kind: effective_kind,
                    checksum_url: None,
                    summary: "Cập nhật giao diện tối trung tính, layout co giãn chuẩn Zalo PC và tự động cập nhật."
                        .to_owned(),
                    notes: "- Hỗ trợ thông báo popup cập nhật và tự động cài đặt ngay trong ứng dụng trên Linux, macOS, Windows.\n- Layout hội thoại 3 chế độ co giãn chuẩn Zalo PC (Normal / SemiCompact / Compact).\n- Tối ưu đồng bộ tin nhắn nền và sửa triệt để lỗi bong bóng gửi liên kết."
                        .to_owned(),
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

    let target = detect_install_target();
    let prefer_installer = prefers_installer_package(&target);
    match parse_release_json_for(
        current_ver,
        &json,
        std::env::consts::OS,
        std::env::consts::ARCH,
        prefer_installer,
    ) {
        Some(info) => (UpdateCheckResult::Available(info), next_etag),
        None => (UpdateCheckResult::UpToDate, next_etag),
    }
}

/// Parses a `SHA256SUMS*.txt` file and returns the expected lowercase hex digest for `asset_name`.
#[must_use]
pub fn parse_sha256_for_asset(manifest: &str, asset_name: &str) -> Option<String> {
    let want = asset_name.trim().to_ascii_lowercase();
    if want.is_empty() {
        return None;
    }
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let hash = parts.next()?.trim();
        let file_part = parts.next()?.trim().trim_start_matches('*');
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let base = Path::new(file_part)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file_part)
            .to_ascii_lowercase();
        if base == want {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Returns the staging directory used to download & unpack updates.
#[must_use]
pub fn update_stage_dir(tag: &str) -> PathBuf {
    let safe_tag: String = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("Zalo-Rust/updates"))
            .unwrap_or_else(|| std::env::temp_dir().join("zalo-rust-updates"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Caches/Zalo-Rust/updates"))
            .unwrap_or_else(|| std::env::temp_dir().join("zalo-rust-updates"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(|c| PathBuf::from(c).join("zalo-rust/updates"))
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".cache/zalo-rust/updates"))
            })
            .unwrap_or_else(|| std::env::temp_dir().join("zalo-rust-updates"))
    };
    base.join(if safe_tag.is_empty() {
        "latest"
    } else {
        &safe_tag
    })
}

/// Downloads the selected update asset with streaming progress, verifies SHA-256
/// if available, installs it according to the current OS & install mode, and
/// launches the updated application / handoff helper.
pub async fn download_and_apply_update<F>(
    info: UpdateInfo,
    mut on_state: F,
) -> Result<ApplyOutcome, String>
where
    F: FnMut(UpdateInstallState),
{
    if !info.package_kind.supports_in_app_install() || info.asset_name.is_empty() {
        return Ok(ApplyOutcome::OpenReleasePage);
    }

    let stage_dir = update_stage_dir(&info.tag);
    let _ = std::fs::remove_dir_all(&stage_dir);
    std::fs::create_dir_all(&stage_dir)
        .map_err(|e| format!("Không tạo được thư mục tạm cập nhật: {e}"))?;

    let part_path = stage_dir.join(format!("{}.part", info.asset_name));
    let file_path = stage_dir.join(&info.asset_name);

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(900))
        .user_agent(format!("Zalo-Rust/{}", crate::APP_VERSION))
        .build()
        .map_err(|e| format!("Không khởi tạo được HTTP client: {e}"))?;

    on_state(UpdateInstallState::Downloading {
        downloaded: 0,
        total: info.asset_size,
    });

    let mut resp = client
        .get(&info.download_url)
        .send()
        .await
        .map_err(|e| format!("Không tải được bản cập nhật: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "Máy chủ tải về trả về lỗi HTTP {}",
            resp.status().as_u16()
        ));
    }

    let total = resp.content_length().or(info.asset_size);
    let mut file = std::fs::File::create(&part_path)
        .map_err(|e| format!("Không tạo được tệp tạm: {e}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut last_emit = std::time::Instant::now();

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Lỗi mạng khi tải bản cập nhật: {e}"))?
    {
        use std::io::Write;
        file.write_all(&chunk)
            .map_err(|e| format!("Lỗi ghi tệp cập nhật: {e}"))?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        if last_emit.elapsed() >= std::time::Duration::from_millis(75) {
            on_state(UpdateInstallState::Downloading { downloaded, total });
            last_emit = std::time::Instant::now();
        }
    }
    drop(file);
    on_state(UpdateInstallState::Downloading { downloaded, total });

    if downloaded == 0 {
        let _ = std::fs::remove_file(&part_path);
        return Err("Tệp cập nhật tải về rỗng (0 byte).".to_owned());
    }

    std::fs::rename(&part_path, &file_path)
        .map_err(|e| format!("Không hoàn tất lưu tệp cập nhật: {e}"))?;

    let actual_sha256 = hex::encode(hasher.finalize());

    if let Some(checksum_url) = info.checksum_url.as_deref() {
        on_state(UpdateInstallState::Verifying);
        if let Ok(sum_resp) = client.get(checksum_url).send().await {
            if sum_resp.status().is_success() {
                if let Ok(text) = sum_resp.text().await {
                    if let Some(expected) = parse_sha256_for_asset(&text, &info.asset_name) {
                        if expected != actual_sha256 {
                            let _ = std::fs::remove_file(&file_path);
                            return Err(
                                "Mã kiểm tra SHA-256 không khớp — tệp tải về có thể bị hỏng."
                                    .to_owned(),
                            );
                        }
                    }
                }
            }
        }
    }

    let target = detect_install_target();
    apply_downloaded_package(&file_path, &stage_dir, info.package_kind, &target, &mut on_state)
}

/// Applies an already-downloaded & verified update package for the detected OS target.
fn apply_downloaded_package<F>(
    package_path: &Path,
    stage_dir: &Path,
    kind: PackageKind,
    target: &InstallTarget,
    on_state: &mut F,
) -> Result<ApplyOutcome, String>
where
    F: FnMut(UpdateInstallState),
{
    match (kind, target) {
        (PackageKind::LinuxTarGz, InstallTarget::LinuxPortable { exe_path, data_dir }) => {
            on_state(UpdateInstallState::Installing {
                message: "Đang giải nén và thay thế tệp chương trình...".to_owned(),
            });
            apply_linux_tarball_to(package_path, stage_dir, exe_path, data_dir)?;
            on_state(UpdateInstallState::Restarting {
                message: "Đang khởi động lại Zalo...".to_owned(),
            });
            relaunch_linux_binary(exe_path)?;
            Ok(ApplyOutcome::QuitToRestart)
        }
        (PackageKind::LinuxTarGz, InstallTarget::LinuxDeb { exe_path }) => {
            // System install fallback when only tarball was published.
            let data_dir = resolve_linux_data_dir(exe_path);
            on_state(UpdateInstallState::Installing {
                message: "Đang giải nén bản cập nhật...".to_owned(),
            });
            apply_linux_tarball_to(package_path, stage_dir, exe_path, &data_dir)?;
            on_state(UpdateInstallState::Restarting {
                message: "Đang khởi động lại Zalo...".to_owned(),
            });
            relaunch_linux_binary(exe_path)?;
            Ok(ApplyOutcome::QuitToRestart)
        }
        (PackageKind::LinuxDeb, _) => {
            on_state(UpdateInstallState::Installing {
                message: "Đang cài đặt gói .deb (cần xác nhận quyền quản trị)...".to_owned(),
            });
            install_linux_deb_pkexec(package_path)?;
            let exe = PathBuf::from("/usr/bin/zalo-gpui");
            let relaunch_exe = if exe.is_file() {
                exe
            } else {
                std::env::current_exe().unwrap_or(exe)
            };
            on_state(UpdateInstallState::Restarting {
                message: "Đang khởi động lại Zalo...".to_owned(),
            });
            relaunch_linux_binary(&relaunch_exe)?;
            Ok(ApplyOutcome::QuitToRestart)
        }
        (PackageKind::MacOsZip, InstallTarget::MacOsAppBundle { app_bundle }) => {
            on_state(UpdateInstallState::Installing {
                message: "Đang giải nén Zalo.app...".to_owned(),
            });
            let extracted_app = extract_macos_zip(package_path, stage_dir)?;
            let script_path = stage_dir.join("apply-macos-update.sh");
            let script = build_macos_swap_script(
                std::process::id(),
                &extracted_app,
                app_bundle,
                stage_dir,
            );
            write_executable_script(&script_path, &script)?;
            on_state(UpdateInstallState::Restarting {
                message: "Đang cập nhật Zalo.app và khởi động lại...".to_owned(),
            });
            spawn_detached_posix_script(&script_path)?;
            Ok(ApplyOutcome::QuitToRestart)
        }
        (PackageKind::WindowsSetupExe, InstallTarget::WindowsInnoSetup { exe_path })
        | (PackageKind::WindowsSetupExe, InstallTarget::WindowsPortable { exe_path }) => {
            on_state(UpdateInstallState::Installing {
                message: "Đang chuẩn bị trình cài đặt Windows...".to_owned(),
            });
            let script_path = stage_dir.join("apply-win-setup.ps1");
            let script = build_windows_setup_script(
                std::process::id(),
                package_path,
                exe_path,
                stage_dir,
            );
            std::fs::write(&script_path, script)
                .map_err(|e| format!("Không ghi được script cập nhật Windows: {e}"))?;
            on_state(UpdateInstallState::Restarting {
                message: "Đang chạy trình cập nhật và khởi động lại Zalo...".to_owned(),
            });
            spawn_detached_powershell(&script_path)?;
            Ok(ApplyOutcome::QuitToRestart)
        }
        (PackageKind::WindowsZip, InstallTarget::WindowsPortable { exe_path })
        | (PackageKind::WindowsZip, InstallTarget::WindowsInnoSetup { exe_path }) => {
            on_state(UpdateInstallState::Installing {
                message: "Đang giải nén gói cập nhật Windows...".to_owned(),
            });
            let extracted_root = extract_windows_zip(package_path, stage_dir)?;
            let target_dir = exe_path
                .parent()
                .ok_or_else(|| "Không xác định được thư mục cài đặt hiện tại.".to_owned())?;
            let script_path = stage_dir.join("apply-win-portable.ps1");
            let script = build_windows_portable_script(
                std::process::id(),
                &extracted_root,
                target_dir,
                exe_path,
                stage_dir,
            );
            std::fs::write(&script_path, script)
                .map_err(|e| format!("Không ghi được script cập nhật Windows: {e}"))?;
            on_state(UpdateInstallState::Restarting {
                message: "Đang thay thế tệp và khởi động lại Zalo...".to_owned(),
            });
            spawn_detached_powershell(&script_path)?;
            Ok(ApplyOutcome::QuitToRestart)
        }
        _ => Ok(ApplyOutcome::OpenReleasePage),
    }
}

/// Extracts a Linux `.tar.gz` archive and atomically installs `bin/zalo-gpui` + `share/zalo-rust`.
pub fn apply_linux_tarball_to(
    archive_path: &Path,
    stage_dir: &Path,
    target_exe: &Path,
    target_data_dir: &Path,
) -> Result<(), String> {
    let extract_dir = stage_dir.join("extracted");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| format!("Không tạo được thư mục giải nén: {e}"))?;

    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(archive_path)
        .arg("-C")
        .arg(&extract_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Không chạy được lệnh tar: {e}"))?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(format!("Giải nén .tar.gz thất bại: {}", stderr.trim()));
    }

    let root = find_dir_containing(&extract_dir, &["bin/zalo-gpui", "zalo-gpui"])
        .ok_or_else(|| "Không tìm thấy tệp thực thi zalo-gpui trong gói tải về.".to_owned())?;
    let new_bin = if root.join("bin/zalo-gpui").is_file() {
        root.join("bin/zalo-gpui")
    } else {
        root.join("zalo-gpui")
    };

    if let Some(parent) = target_exe.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Không tạo được thư mục {}: {e}", parent.display()))?;
    }
    atomic_replace_file(&new_bin, target_exe, true)?;

    let new_assets = root.join("share/zalo-rust");
    if new_assets.is_dir() {
        copy_dir_atomic(&new_assets, target_data_dir)?;
    }
    Ok(())
}

/// Installs a `.deb` package via `pkexec apt-get install` (or `pkexec dpkg -i`).
fn install_linux_deb_pkexec(deb_path: &Path) -> Result<(), String> {
    let has_apt = Path::new("/usr/bin/apt-get").is_file();
    let mut cmd = Command::new("pkexec");
    if has_apt {
        cmd.args(["apt-get", "install", "-y", "--reinstall"])
            .arg(deb_path);
    } else {
        cmd.args(["dpkg", "-i"]).arg(deb_path);
    }
    let out = cmd.output().map_err(|e| {
        format!("Không chạy được pkexec để cài đặt gói .deb ({e}). Hãy thử mở trang tải thủ công.")
    })?;
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        if code == 126 || code == 127 {
            return Err("Đã hủy cấp quyền quản trị để cài đặt gói .deb.".to_owned());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            format!("Cài đặt gói .deb thất bại (mã lỗi {code}).")
        } else {
            format!("Cài đặt gói .deb thất bại: {detail}")
        });
    }
    Ok(())
}

/// Launches the freshly replaced Linux binary as a detached child process.
fn relaunch_linux_binary(exe_path: &Path) -> Result<(), String> {
    Command::new(exe_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Không khởi động lại được Zalo: {e}"))?;
    Ok(())
}

/// Extracts `Zalo-*.zip` on macOS using `/usr/bin/ditto` and returns the extracted `Zalo.app` path.
fn extract_macos_zip(zip_path: &Path, stage_dir: &Path) -> Result<PathBuf, String> {
    let extract_dir = stage_dir.join("extracted");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| format!("Không tạo được thư mục giải nén: {e}"))?;

    let out = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip_path)
        .arg(&extract_dir)
        .output()
        .map_err(|e| format!("Không chạy được ditto để giải nén bản cập nhật: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("Giải nén gói macOS thất bại: {}", stderr.trim()));
    }
    let app = find_macos_app_in(&extract_dir)
        .ok_or_else(|| "Không tìm thấy Zalo.app trong gói cập nhật.".to_owned())?;
    let _ = Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&app)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(app)
}

/// Extracts a Windows portable `.zip` archive via PowerShell `Expand-Archive`.
fn extract_windows_zip(zip_path: &Path, stage_dir: &Path) -> Result<PathBuf, String> {
    let extract_dir = stage_dir.join("extracted");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| format!("Không tạo được thư mục giải nén: {e}"))?;

    let cmd_str = format!(
        "Expand-Archive -LiteralPath {} -DestinationPath {} -Force",
        ps_single_quote(&zip_path.to_string_lossy()),
        ps_single_quote(&extract_dir.to_string_lossy()),
    );
    let out = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &cmd_str,
        ])
        .output()
        .map_err(|e| format!("Không chạy được PowerShell để giải nén gói cập nhật: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("Giải nén gói Windows thất bại: {}", stderr.trim()));
    }
    find_dir_containing(&extract_dir, &["zalo-gpui.exe"])
        .ok_or_else(|| "Không tìm thấy zalo-gpui.exe trong gói cập nhật Windows.".to_owned())
}

fn find_macos_app_in(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join("Zalo.app");
    if direct.join("Contents/MacOS/zalo-gpui").is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.extension().and_then(|e| e.to_str()) == Some("app") {
            if path.join("Contents/MacOS/zalo-gpui").is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn find_dir_containing(root: &Path, markers: &[&str]) -> Option<PathBuf> {
    if markers.iter().any(|m| root.join(m).is_file()) {
        return Some(root.to_path_buf());
    }
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && markers.iter().any(|m| path.join(m).is_file()) {
            return Some(path);
        }
    }
    None
}

fn atomic_replace_file(src: &Path, dst: &Path, executable: bool) -> Result<(), String> {
    let parent = dst
        .parent()
        .ok_or_else(|| format!("Đường dẫn đích không hợp lệ: {}", dst.display()))?;
    let name = dst
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("zalo-gpui");
    let tmp = parent.join(format!(".{name}.update.tmp"));
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(src, &tmp)
        .map_err(|e| format!("Không sao chép được {}: {e}", dst.display()))?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Không đặt quyền thực thi cho {}: {e}", dst.display()))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    std::fs::rename(&tmp, dst)
        .map_err(|e| format!("Không thay thế được {}: {e}", dst.display()))?;
    Ok(())
}

fn copy_dir_atomic(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst)
        .map_err(|e| format!("Không tạo được thư mục {}: {e}", dst.display()))?;
    let entries = std::fs::read_dir(src)
        .map_err(|e| format!("Không đọc được thư mục {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        if ty.is_dir() {
            copy_dir_atomic(&from, &to)?;
        } else if ty.is_file() {
            atomic_replace_file(&from, &to, false)?;
        }
    }
    Ok(())
}

/// Safely quotes a string for POSIX `/bin/sh`.
#[must_use]
pub fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Safely quotes a string for PowerShell single-quoted literals.
#[must_use]
pub fn ps_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Builds the detached macOS helper script that swaps `Zalo.app` once `pid` exits and relaunches it.
#[must_use]
pub fn build_macos_swap_script(
    pid: u32,
    new_app: &Path,
    target_app: &Path,
    stage_dir: &Path,
) -> String {
    let q_new = shell_single_quote(&new_app.to_string_lossy());
    let q_dst = shell_single_quote(&target_app.to_string_lossy());
    let q_parent = shell_single_quote(
        &target_app
            .parent()
            .unwrap_or_else(|| Path::new("/Applications"))
            .to_string_lossy(),
    );
    let q_stage = shell_single_quote(&stage_dir.to_string_lossy());
    format!(
        "#!/bin/sh\n\
         set -e\n\
         PID={pid}\n\
         while kill -0 \"$PID\" 2>/dev/null; do sleep 0.2; done\n\
         NEW_APP={q_new}\n\
         DST_APP={q_dst}\n\
         DST_PARENT={q_parent}\n\
         if [ -w \"$DST_PARENT\" ]; then\n\
           mkdir -p \"$DST_PARENT\"\n\
           rm -rf \"${{DST_APP}}.bak\"\n\
           if [ -d \"$DST_APP\" ]; then mv \"$DST_APP\" \"${{DST_APP}}.bak\"; fi\n\
           if mv \"$NEW_APP\" \"$DST_APP\"; then\n\
             rm -rf \"${{DST_APP}}.bak\"\n\
           else\n\
             if [ -d \"${{DST_APP}}.bak\" ]; then mv \"${{DST_APP}}.bak\" \"$DST_APP\"; fi\n\
             exit 1\n\
           fi\n\
         else\n\
           /usr/bin/osascript -e \"do shell script \\\"rm -rf '${{DST_APP}}.bak' && ([ -d '${{DST_APP}}' ] && mv '${{DST_APP}}' '${{DST_APP}}.bak' || true) && mv '${{NEW_APP}}' '${{DST_APP}}' && rm -rf '${{DST_APP}}.bak'\\\" with administrator privileges\"\n\
         fi\n\
         /usr/bin/xattr -dr com.apple.quarantine \"$DST_APP\" 2>/dev/null || true\n\
         /usr/bin/open -n \"$DST_APP\"\n\
         rm -rf {q_stage}\n"
    )
}

/// Builds the detached Windows PowerShell script for silent Inno Setup upgrade + relaunch.
#[must_use]
pub fn build_windows_setup_script(
    pid: u32,
    setup_exe: &Path,
    relaunch_exe: &Path,
    stage_dir: &Path,
) -> String {
    let q_setup = ps_single_quote(&setup_exe.to_string_lossy());
    let q_exe = ps_single_quote(&relaunch_exe.to_string_lossy());
    let q_stage = ps_single_quote(&stage_dir.to_string_lossy());
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         try {{ Wait-Process -Id {pid} -Timeout 30 -ErrorAction SilentlyContinue }} catch {{}}\n\
         Start-Sleep -Milliseconds 300\n\
         $p = Start-Process -FilePath {q_setup} -ArgumentList '/SILENT','/SUPPRESSMSGBOXES','/NORESTART','/CLOSEAPPLICATIONS' -Wait -PassThru\n\
         if ($p.ExitCode -eq 0 -and (Test-Path {q_exe})) {{\n\
           Start-Process -FilePath {q_exe}\n\
         }}\n\
         Remove-Item -LiteralPath {q_stage} -Recurse -Force -ErrorAction SilentlyContinue\n"
    )
}

/// Builds the detached Windows PowerShell script for atomic portable folder replacement + relaunch.
#[must_use]
pub fn build_windows_portable_script(
    pid: u32,
    extracted_root: &Path,
    target_dir: &Path,
    relaunch_exe: &Path,
    stage_dir: &Path,
) -> String {
    let q_src = ps_single_quote(&extracted_root.to_string_lossy());
    let q_dst = ps_single_quote(&target_dir.to_string_lossy());
    let q_exe = ps_single_quote(&relaunch_exe.to_string_lossy());
    let q_stage = ps_single_quote(&stage_dir.to_string_lossy());
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         try {{ Wait-Process -Id {pid} -Timeout 30 -ErrorAction SilentlyContinue }} catch {{}}\n\
         Start-Sleep -Milliseconds 400\n\
         Copy-Item -Path (Join-Path {q_src} '*') -Destination {q_dst} -Recurse -Force\n\
         if (Test-Path {q_exe}) {{\n\
           Start-Process -FilePath {q_exe}\n\
         }}\n\
         Remove-Item -LiteralPath {q_stage} -Recurse -Force -ErrorAction SilentlyContinue\n"
    )
}

fn write_executable_script(path: &Path, content: &str) -> Result<(), String> {
    std::fs::write(path, content)
        .map_err(|e| format!("Không ghi được script cập nhật: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Không đặt quyền thực thi script: {e}"))?;
    }
    Ok(())
}

fn spawn_detached_posix_script(script_path: &Path) -> Result<(), String> {
    Command::new("/bin/sh")
        .arg(script_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Không khởi chạy được trình cập nhật nền: {e}"))?;
    Ok(())
}

fn spawn_detached_powershell(script_path: &Path) -> Result<(), String> {
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-WindowStyle",
        "Hidden",
        "-File",
    ])
    .arg(script_path)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    cmd.spawn()
        .map_err(|e| format!("Không khởi chạy được trình cập nhật Windows: {e}"))?;
    Ok(())
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
            "Phiên bản hiện tại {}. Mở hộp thoại cập nhật để tải và cài đặt tự động.",
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
    fn parses_github_release_and_picks_os_arch_and_mode_assets() {
        let payload = json!({
            "tag_name": "v0.3.4",
            "html_url": "https://github.com/nct89/Zalo-Rust/releases/tag/v0.3.4",
            "draft": false,
            "prerelease": false,
            "body": "## Highlights\n- Auto update popup & installer.\n- Responsive compact layout.",
            "assets": [
                {
                    "name": "zalo-linux_0.3.4_amd64.deb",
                    "size": 24000000,
                    "browser_download_url": "https://example.invalid/zalo.deb"
                },
                {
                    "name": "zalo-linux-0.3.4-linux-x86_64.tar.gz",
                    "size": 25000000,
                    "browser_download_url": "https://example.invalid/zalo-linux.tar.gz"
                },
                {
                    "name": "SHA256SUMS.txt",
                    "browser_download_url": "https://example.invalid/SHA256SUMS.txt"
                },
                {
                    "name": "Zalo-0.3.4-macos-aarch64-apple-darwin.zip",
                    "size": 28000000,
                    "browser_download_url": "https://example.invalid/zalo-mac-arm64.zip"
                },
                {
                    "name": "Zalo-0.3.4-macos-x86_64-apple-darwin.zip",
                    "size": 28500000,
                    "browser_download_url": "https://example.invalid/zalo-mac-x64.zip"
                },
                {
                    "name": "SHA256SUMS-macos.txt",
                    "browser_download_url": "https://example.invalid/SHA256SUMS-macos.txt"
                },
                {
                    "name": "Zalo-0.3.4-windows-x86_64-setup.exe",
                    "size": 27000000,
                    "browser_download_url": "https://example.invalid/zalo-setup.exe"
                },
                {
                    "name": "Zalo-0.3.4-windows-x86_64.zip",
                    "size": 26500000,
                    "browser_download_url": "https://example.invalid/zalo-win.zip"
                },
                {
                    "name": "SHA256SUMS-windows.txt",
                    "browser_download_url": "https://example.invalid/SHA256SUMS-windows.txt"
                }
            ]
        });

        let linux_deb =
            parse_release_json_for("0.3.3", &payload, "linux", "x86_64", true).expect("deb");
        assert_eq!(linux_deb.package_kind, PackageKind::LinuxDeb);
        assert_eq!(linux_deb.download_url, "https://example.invalid/zalo.deb");
        assert_eq!(
            linux_deb.checksum_url.as_deref(),
            Some("https://example.invalid/SHA256SUMS.txt")
        );

        let linux_portable =
            parse_release_json_for("0.3.3", &payload, "linux", "x86_64", false).expect("tar");
        assert_eq!(linux_portable.package_kind, PackageKind::LinuxTarGz);
        assert_eq!(
            linux_portable.download_url,
            "https://example.invalid/zalo-linux.tar.gz"
        );

        let mac_arm =
            parse_release_json_for("0.3.3", &payload, "macos", "aarch64", false).expect("mac arm");
        assert_eq!(mac_arm.package_kind, PackageKind::MacOsZip);
        assert_eq!(
            mac_arm.download_url,
            "https://example.invalid/zalo-mac-arm64.zip"
        );
        assert_eq!(
            mac_arm.checksum_url.as_deref(),
            Some("https://example.invalid/SHA256SUMS-macos.txt")
        );

        let mac_x64 =
            parse_release_json_for("0.3.3", &payload, "macos", "x86_64", false).expect("mac x64");
        assert_eq!(
            mac_x64.download_url,
            "https://example.invalid/zalo-mac-x64.zip"
        );

        let win_setup =
            parse_release_json_for("0.3.3", &payload, "windows", "x86_64", true).expect("setup");
        assert_eq!(win_setup.package_kind, PackageKind::WindowsSetupExe);
        assert_eq!(
            win_setup.download_url,
            "https://example.invalid/zalo-setup.exe"
        );
        assert_eq!(
            win_setup.checksum_url.as_deref(),
            Some("https://example.invalid/SHA256SUMS-windows.txt")
        );

        let win_zip =
            parse_release_json_for("0.3.3", &payload, "windows", "x86_64", false).expect("zip");
        assert_eq!(win_zip.package_kind, PackageKind::WindowsZip);
        assert_eq!(win_zip.download_url, "https://example.invalid/zalo-win.zip");
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

    #[test]
    fn parses_sha256_manifest_formats() {
        let manifest = "\
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  zalo-linux_0.3.4_amd64.deb
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef *dist/Zalo-0.3.4-windows-x86_64-setup.exe
";
        assert_eq!(
            parse_sha256_for_asset(manifest, "zalo-linux_0.3.4_amd64.deb").as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(
            parse_sha256_for_asset(manifest, "Zalo-0.3.4-windows-x86_64-setup.exe").as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert!(parse_sha256_for_asset(manifest, "missing.tar.gz").is_none());
    }

    #[test]
    fn detects_macos_app_bundle_and_generates_safe_platform_scripts() {
        let exe = Path::new("/Applications/Zalo.app/Contents/MacOS/zalo-gpui");
        assert_eq!(
            macos_app_bundle_from_exe(exe),
            Some(PathBuf::from("/Applications/Zalo.app"))
        );

        let mac_script = build_macos_swap_script(
            4242,
            Path::new("/tmp/stage/Zalo.app"),
            Path::new("/Applications/Zalo.app"),
            Path::new("/tmp/stage"),
        );
        assert!(mac_script.contains("PID=4242"));
        assert!(mac_script.contains("/usr/bin/open -n"));
        assert!(mac_script.contains("com.apple.quarantine"));

        let win_setup = build_windows_setup_script(
            9001,
            Path::new(r"C:\Temp\Zalo-0.3.4-windows-x86_64-setup.exe"),
            Path::new(r"C:\Users\me\AppData\Local\Programs\Zalo-Rust\zalo-gpui.exe"),
            Path::new(r"C:\Temp\stage"),
        );
        assert!(win_setup.contains("Wait-Process -Id 9001"));
        assert!(win_setup.contains("/SILENT"));
        assert!(win_setup.contains("/CLOSEAPPLICATIONS"));

        let win_portable = build_windows_portable_script(
            9002,
            Path::new(r"C:\Temp\stage\extracted\Zalo-0.3.4-windows-x86_64"),
            Path::new(r"D:\Apps\Zalo"),
            Path::new(r"D:\Apps\Zalo\zalo-gpui.exe"),
            Path::new(r"C:\Temp\stage"),
        );
        assert!(win_portable.contains("Wait-Process -Id 9002"));
        assert!(win_portable.contains("Copy-Item"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn applies_linux_tarball_atomically_to_target_dir() {
        let base = std::env::temp_dir().join(format!(
            "zalo-updater-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let pkg_root = base.join("pkg/zalo-linux-0.3.4-linux-x86_64");
        std::fs::create_dir_all(pkg_root.join("bin")).unwrap();
        std::fs::create_dir_all(pkg_root.join("share/zalo-rust")).unwrap();
        std::fs::write(pkg_root.join("bin/zalo-gpui"), b"NEW_BINARY_V034").unwrap();
        std::fs::write(pkg_root.join("share/zalo-rust/protocol.json"), b"{\"v\":2}").unwrap();

        let archive = base.join("zalo-linux-0.3.4-linux-x86_64.tar.gz");
        let st = Command::new("tar")
            .args(["-C"])
            .arg(base.join("pkg"))
            .args(["-czf"])
            .arg(&archive)
            .arg("zalo-linux-0.3.4-linux-x86_64")
            .status()
            .unwrap();
        assert!(st.success());

        let target_exe = base.join("installed/bin/zalo-gpui");
        let target_data = base.join("installed/share/zalo-rust");
        std::fs::create_dir_all(target_exe.parent().unwrap()).unwrap();
        std::fs::write(&target_exe, b"OLD_BINARY_V033").unwrap();

        let stage = base.join("stage");
        apply_linux_tarball_to(&archive, &stage, &target_exe, &target_data).unwrap();

        assert_eq!(std::fs::read(&target_exe).unwrap(), b"NEW_BINARY_V034");
        assert_eq!(
            std::fs::read(target_data.join("protocol.json")).unwrap(),
            b"{\"v\":2}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
