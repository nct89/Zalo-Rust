//! Native Linux Distro Scanner, Package Installer & OCR Pack Downloader.
//!
//! Launches with `zalo-gpui --install` (or `--setup`). Detects the host Linux
//! distribution (`/etc/os-release`), inspects runtime dependencies and existing
//! installs, installs the optimized `zalo-gpui` binary, desktop entry, icons,
//! runtime fonts and ringtones for the user or distro package manager, and
//! optionally streams the OCR runtime + models archive with SHA-256 verification.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, rgb, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::color::token_rgb;
use crate::fonts::FAMILY_SEMIBOLD;
use crate::window_frame::{wrap_csd, CsdCorners};

/// Public Cloudflare R2 OCR manifest URL (bucket `zalo` behind `dl.lza.lol`,
/// overridable via `ZALO_OCR_MANIFEST_URL`).
pub const OCR_MANIFEST_URL: &str = "https://dl.lza.lol/zalo-linux/ocr/manifest.json";

/// Linux distribution family detected from `/etc/os-release`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistroFamily {
    /// Debian, Ubuntu, Linux Mint, Pop!_OS, etc.
    Debian,
    /// Fedora, RHEL, CentOS, Rocky, AlmaLinux, Nobara.
    Fedora,
    /// openSUSE Tumbleweed / Leap, SLES.
    OpenSuse,
    /// Arch Linux, Manjaro, EndeavourOS, CachyOS.
    Arch,
    /// Other / generic Linux distribution.
    Generic,
}

impl DistroFamily {
    /// Native system package format label for this distro family.
    #[must_use]
    pub const fn package_label(self) -> &'static str {
        match self {
            Self::Debian => "Gói .deb (apt / dpkg)",
            Self::Fedora => "Gói .rpm (dnf)",
            Self::OpenSuse => "Gói .rpm (zypper)",
            Self::Arch => "Gói Arch (pacman)",
            Self::Generic => "Binary chuẩn XDG (~/.local)",
        }
    }

    /// Suggested install command prefix if system packages are missing.
    #[must_use]
    pub const fn install_cmd_prefix(self) -> Option<&'static str> {
        match self {
            Self::Debian => Some("apt-get install -y"),
            Self::Fedora => Some("dnf install -y"),
            Self::OpenSuse => Some("zypper --non-interactive install"),
            Self::Arch => Some("pacman -S --needed --noconfirm"),
            Self::Generic => None,
        }
    }
}

/// One system dependency checked for the active distro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepCheck {
    /// Human-readable capability name.
    pub label: String,
    /// Package name in the current distro (when known).
    pub package: String,
    /// Whether the dependency is already satisfied on the host.
    pub installed: bool,
    /// Whether this item is strictly needed vs optional.
    pub required: bool,
}

/// Complete host scan report shown in the installer UI.
#[derive(Clone, Debug)]
pub struct SystemScan {
    /// Pretty OS name (from `PRETTY_NAME`).
    pub pretty_name: String,
    /// `ID` from `/etc/os-release`.
    pub distro_id: String,
    /// Classified distribution family.
    pub family: DistroFamily,
    /// CPU architecture (`x86_64`, `aarch64`, ...).
    pub arch: String,
    /// Display backend & desktop session (`X11 · XFCE`, `Wayland · GNOME`, ...).
    pub session_summary: String,
    /// Existing installed binary path if found.
    pub existing_binary: Option<PathBuf>,
    /// Existing installed OCR dir if found.
    pub existing_ocr: Option<PathBuf>,
    /// Target user OCR dir (`$XDG_DATA_HOME/zalo-rust/ocr`).
    pub target_ocr_dir: PathBuf,
    /// Checked dependencies for this distro.
    pub deps: Vec<DepCheck>,
    /// Whether `dpkg-deb` is available to build a local `.deb`.
    pub can_build_deb: bool,
    /// Whether `pkexec` is available for system-wide install.
    pub has_pkexec: bool,
}

/// Distro package names for missing dependencies.
#[allow(dead_code)]
impl SystemScan {
    /// Distro package names for missing required or recommended dependencies.
    #[must_use]
    pub fn missing_packages(&self) -> Vec<String> {
        let mut pkgs = Vec::new();
        for dep in &self.deps {
            if !dep.installed && !dep.package.is_empty() && !pkgs.contains(&dep.package) {
                pkgs.push(dep.package.clone());
            }
        }
        pkgs
    }

    /// Required distro packages that are currently missing on this host.
    #[must_use]
    pub fn missing_required_packages(&self) -> Vec<String> {
        let mut pkgs = Vec::new();
        for dep in &self.deps {
            if dep.required && !dep.installed && !dep.package.is_empty() && !pkgs.contains(&dep.package)
            {
                pkgs.push(dep.package.clone());
            }
        }
        pkgs
    }
}

/// Remote `manifest.json` describing the OCR tarball.
#[derive(Clone, Debug)]
pub struct OcrManifest {
    /// Version tag.
    #[allow(dead_code)]
    pub version: String,
    /// Target platform (`linux-x86_64`).
    pub platform: String,
    /// Public download URL for the `.tar.zst` archive.
    pub url: String,
    /// Expected lowercase hex SHA-256 digest of the `.tar.zst` archive.
    pub sha256: String,
    /// Compressed archive size in bytes.
    pub size: u64,
}

impl OcrManifest {
    fn from_json(v: &Value) -> Result<Self, String> {
        let str_field = |k: &str| -> Result<String, String> {
            v.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("manifest.json thiếu trường `{k}`"))
        };
        Ok(Self {
            version: str_field("version")?,
            platform: str_field("platform")?,
            url: str_field("url")?,
            sha256: str_field("sha256")?,
            size: v.get("size").and_then(Value::as_u64).unwrap_or(0),
        })
    }
}

/// Live progress callbacks emitted during installation.
#[derive(Clone, Debug)]
pub enum InstallEvent {
    /// Step progress update (`step`: `0` = Zalo install, `1` = OCR download/install).
    Step {
        /// Active pipeline step (`0` = Zalo, `1` = OCR).
        step: usize,
        /// Step progress `0.0..=1.0`.
        progress: f32,
        /// Active file or command being installed/executed.
        status: String,
    },
    /// Streaming R2 OCR download progress.
    OcrDownload {
        /// Archive filename being downloaded.
        archive: String,
        /// Downloaded bytes so far.
        done_bytes: u64,
        /// Total archive bytes (from `Content-Length` or `manifest.size`).
        total_bytes: u64,
        /// Current throughput in bytes per second.
        speed_bps: u64,
    },
    /// Informational trace line (ignored by minimal progress bar UI).
    #[allow(dead_code)]
    Log(String),
    /// Pipeline completed successfully (installed binary path).
    Done {
        /// Installed executable path.
        exe_path: PathBuf,
    },
    /// Pipeline aborted with an error message.
    Error(String),
}

/// Parse `/etc/os-release` key-value pairs cleanly.
#[must_use]
pub fn parse_os_release(content: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let val = v.trim();
        let unquoted = if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2)
            || (val.starts_with('\'') && val.ends_with('\'') && val.len() >= 2)
        {
            &val[1..val.len() - 1]
        } else {
            val
        };
        map.insert(k.trim().to_owned(), unquoted.to_owned());
    }
    map
}

/// Classify a Linux distro from its `ID` and `ID_LIKE` fields.
#[must_use]
pub fn classify_distro(id: &str, id_like: &str) -> DistroFamily {
    let haystack = format!("{} {}", id.to_ascii_lowercase(), id_like.to_ascii_lowercase());
    let has = |token: &str| haystack.split_whitespace().any(|w| w == token);
    if has("debian")
        || has("ubuntu")
        || has("linuxmint")
        || has("pop")
        || has("zorin")
        || has("elementary")
        || has("kali")
        || has("mx")
    {
        return DistroFamily::Debian;
    }
    if has("fedora")
        || has("rhel")
        || has("centos")
        || has("rocky")
        || has("almalinux")
        || has("nobara")
    {
        return DistroFamily::Fedora;
    }
    if has("opensuse") || has("opensuse-tumbleweed") || has("opensuse-leap") || has("suse") || has("sles") {
        return DistroFamily::OpenSuse;
    }
    if has("arch") || has("manjaro") || has("endeavouros") || has("cachyos") || has("garuda") {
        return DistroFamily::Arch;
    }
    DistroFamily::Generic
}

fn bin_in_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

fn pkg_installed(family: DistroFamily, pkg: &str) -> Option<bool> {
    if pkg.is_empty() {
        return None;
    }
    match family {
        DistroFamily::Debian if bin_in_path("dpkg-query") => {
            let out = Command::new("dpkg-query")
                .args(["-W", "-f=${db:Status-Status}", pkg])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            Some(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "installed")
        }
        DistroFamily::Fedora | DistroFamily::OpenSuse if bin_in_path("rpm") => {
            let st = Command::new("rpm")
                .args(["-q", pkg])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()?;
            Some(st.success())
        }
        DistroFamily::Arch if bin_in_path("pacman") => {
            let st = Command::new("pacman")
                .args(["-Q", pkg])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()?;
            Some(st.success())
        }
        _ => None,
    }
}

struct DepSpec<'a> {
    label: &'a str,
    deb: &'a str,
    rpm: &'a str,
    arch: &'a str,
    fallback_bin: Option<&'a str>,
    fallback_lib: Option<&'a str>,
    required: bool,
}

fn check_dep(family: DistroFamily, spec: DepSpec<'_>) -> DepCheck {
    let package = match family {
        DistroFamily::Debian => spec.deb,
        DistroFamily::Fedora | DistroFamily::OpenSuse => spec.rpm,
        DistroFamily::Arch => spec.arch,
        DistroFamily::Generic => "",
    };
    let installed = pkg_installed(family, package).unwrap_or_else(|| {
        spec.fallback_bin.is_some_and(bin_in_path)
            || spec.fallback_lib.is_some_and(lib_exists)
    });
    DepCheck {
        label: spec.label.to_owned(),
        package: package.to_owned(),
        installed,
        required: spec.required,
    }
}

fn lib_exists(soname: &str) -> bool {
    [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/lib/x86_64-linux-gnu",
        "/lib64",
    ]
    .iter()
    .any(|dir| Path::new(dir).join(soname).exists())
}

/// Default OCR install path (`$XDG_DATA_HOME/zalo-rust/ocr` or `~/.local/share/zalo-rust/ocr`).
#[must_use]
pub fn default_ocr_dir() -> PathBuf {
    zalo_shell::data_home()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("zalo-rust")
        .join("ocr")
}

fn user_bin_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/bin/zalo-gpui");
    }
    PathBuf::from("/tmp/zalo-gpui")
}

/// Folder of the user-local binary, when that file is already installed.
#[must_use]
pub fn installed_bin_dir() -> Option<PathBuf> {
    let path = user_bin_path();
    let dir = path.parent()?.to_path_buf();
    path.is_file().then_some(dir)
}

/// Scan `/etc/os-release`, session type, existing install, and distro dependencies.
#[must_use]
pub fn scan_system() -> SystemScan {
    let os_raw = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let os = parse_os_release(&os_raw);
    let distro_id = os.get("ID").cloned().unwrap_or_else(|| "linux".to_owned());
    let id_like = os.get("ID_LIKE").cloned().unwrap_or_default();
    let pretty_name = os
        .get("PRETTY_NAME")
        .cloned()
        .unwrap_or_else(|| distro_id.clone());
    let family = classify_distro(&distro_id, &id_like);
    let arch = std::env::consts::ARCH.to_owned();

    let display_srv = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        "Wayland"
    } else if std::env::var_os("DISPLAY").is_some() {
        "X11"
    } else {
        "Headless"
    };
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Desktop".to_owned());
    let session_summary = format!("{display_srv} · {desktop}");

    let user_bin = user_bin_path();
    let existing_binary = [user_bin, PathBuf::from("/usr/bin/zalo-gpui"), PathBuf::from("/usr/local/bin/zalo-gpui")]
        .into_iter()
        .find(|p| p.is_file());
    let existing_ocr = zocr::find_ocr_dir()
        .and_then(|install| install.manifest.parent().map(Path::to_path_buf))
        .map(|p| {
            if p.file_name().is_some_and(|n| n == "zocr") {
                p.parent().map_or_else(|| p.clone(), Path::to_path_buf)
            } else {
                p
            }
        });

    let mut deps = vec![
        check_dep(
            family,
            DepSpec {
                label: "Hiển thị X11 / Wayland",
                deb: if display_srv == "Wayland" {
                    "libwayland-client0"
                } else {
                    "libx11-6"
                },
                rpm: if display_srv == "Wayland" {
                    "wayland"
                } else {
                    "libX11"
                },
                arch: if display_srv == "Wayland" {
                    "wayland"
                } else {
                    "libx11"
                },
                fallback_bin: None,
                fallback_lib: Some(if display_srv == "Wayland" {
                    "libwayland-client.so.0"
                } else {
                    "libX11.so.6"
                }),
                required: true,
            },
        ),
        check_dep(
            family,
            DepSpec {
                label: "Thông báo desktop (libnotify)",
                deb: "libnotify4",
                rpm: "libnotify",
                arch: "libnotify",
                fallback_bin: Some("notify-send"),
                fallback_lib: Some("libnotify.so.4"),
                required: false,
            },
        ),
        check_dep(
            family,
            DepSpec {
                label: "XDG Desktop Portal (chọn file / chụp màn hình)",
                deb: "xdg-desktop-portal",
                rpm: "xdg-desktop-portal",
                arch: "xdg-desktop-portal",
                fallback_bin: None,
                fallback_lib: None,
                required: false,
            },
        ),
        check_dep(
            family,
            DepSpec {
                label: "Âm thanh & micro (PipeWire / PulseAudio)",
                deb: "libpulse0",
                rpm: "pulseaudio-libs",
                arch: "libpulse",
                fallback_bin: Some("pactl"),
                fallback_lib: Some("libpulse.so.0"),
                required: true,
            },
        ),
        check_dep(
            family,
            DepSpec {
                label: "Chuông báo & xem video nội bộ (ffmpeg / ffplay)",
                deb: "ffmpeg",
                rpm: "ffmpeg-free",
                arch: "ffmpeg",
                fallback_bin: Some("ffplay"),
                fallback_lib: None,
                required: false,
            },
        ),
    ];
    if desktop.to_ascii_lowercase().contains("gnome") {
        deps.push(check_dep(
            family,
            DepSpec {
                label: "Khay hệ thống GNOME (AppIndicator)",
                deb: "gir1.2-ayatanaappindicator3-0.1",
                rpm: "libappindicator-gtk3",
                arch: "libayatana-appindicator",
                fallback_bin: None,
                fallback_lib: Some("libayatana-appindicator3.so.1"),
                required: false,
            },
        ));
    }

    SystemScan {
        pretty_name,
        distro_id,
        family,
        arch,
        session_summary,
        existing_binary,
        existing_ocr,
        target_ocr_dir: default_ocr_dir(),
        deps,
        can_build_deb: family == DistroFamily::Debian && bin_in_path("dpkg-deb"),
        has_pkexec: bin_in_path("pkexec"),
    }
}

fn current_source_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Không xác định được file thực thi: {e}"))?;
    if exe.is_file() {
        return Ok(exe);
    }
    Err("Không tìm thấy binary nguồn để cài đặt".to_owned())
}

fn copy_file_atomic(src: &Path, dst: &Path, executable: bool) -> Result<(), String> {
    let Some(parent) = dst.parent() else {
        return Err(format!("Đường dẫn không hợp lệ: {}", dst.display()));
    };
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let tmp = dst.with_extension("tmp");
    std::fs::copy(src, &tmp).map_err(|e| format!("Chép {} thất bại: {e}", dst.display()))?;
    if executable {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        }
    }
    std::fs::rename(&tmp, dst).map_err(|e| format!("Lưu {} thất bại: {e}", dst.display()))
}

fn short_user_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        if let Ok(rel) = path.strip_prefix(Path::new(&home)) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

fn copy_asset_group(
    sources: Vec<PathBuf>,
    dst_dir: &Path,
    rel_prefix: &str,
    pct_start: f32,
    pct_end: f32,
    on_status: &mut dyn FnMut(f32, String),
) -> Result<(), String> {
    let total = sources.len().max(1) as f32;
    for (idx, src) in sources.into_iter().enumerate() {
        let Some(name) = src.file_name() else {
            continue;
        };
        let dst = dst_dir.join(name);
        let pct = pct_start + ((idx + 1) as f32 / total) * (pct_end - pct_start);
        on_status(pct, format!("Đang cài: {rel_prefix}/{}", name.to_string_lossy()));
        if src != dst {
            copy_file_atomic(&src, &dst, false)?;
        }
    }
    Ok(())
}

fn ensure_missing_required_deps(
    scan: &SystemScan,
    on_status: &mut dyn FnMut(f32, String),
) -> Result<(), String> {
    let missing = scan.missing_required_packages();
    if missing.is_empty() || !scan.has_pkexec {
        on_status(
            0.12,
            format!("Kiểm tra gói ({}): đủ thư viện", scan.family.package_label()),
        );
        return Ok(());
    }
    let Some(prefix) = scan.family.install_cmd_prefix() else {
        return Ok(());
    };
    let script = format!("{prefix} {}", missing.join(" "));
    on_status(0.14, format!("Đang chạy: pkexec {script}"));
    let st = Command::new("pkexec")
        .args(["sh", "-c", &script])
        .status()
        .map_err(|e| format!("Không chạy được pkexec: {e}"))?;
    if !st.success() {
        return Err(format!(
            "Không cài được thư viện hệ thống cần thiết ({})",
            missing.join(", ")
        ));
    }
    Ok(())
}

fn install_user_local(
    source_exe: &Path,
    on_status: &mut dyn FnMut(f32, String),
) -> Result<PathBuf, String> {
    let data_home = zalo_shell::data_home().ok_or("Không tìm thấy $HOME / $XDG_DATA_HOME")?;
    let target_exe = user_bin_path();
    on_status(0.22, format!("Đang cài: {}", short_user_path(&target_exe)));
    if source_exe != target_exe {
        copy_file_atomic(source_exe, &target_exe, true)?;
    }

    let asset_root = data_home.join("zalo-rust");
    copy_asset_group(
        crate::fonts::runtime_font_paths(),
        &asset_root.join("fonts"),
        "fonts",
        0.35,
        0.58,
        on_status,
    )?;
    copy_asset_group(
        crate::call::runtime_sound_paths(),
        &asset_root.join("zcall-sound"),
        "zcall-sound",
        0.58,
        0.70,
        on_status,
    )?;
    let wasm_files: Vec<PathBuf> = [
        "libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm",
        "libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm",
    ]
    .into_iter()
    .filter_map(crate::ws_rpc::locate_wasm)
    .collect();
    copy_asset_group(
        wasm_files,
        &asset_root.join("wasm"),
        "wasm",
        0.70,
        0.75,
        on_status,
    )?;
    copy_asset_group(
        crate::emoji::runtime_emoji_paths(),
        &asset_root.join("emoji"),
        "emoji",
        0.75,
        0.78,
        on_status,
    )?;
    let images_src = crate::media::images_dir();
    if images_src.is_dir() && images_src != asset_root.join("images") {
        on_status(0.79, "Đang cài: ảnh giao diện Zalo".to_owned());
        copy_dir_recursive(&images_src, &asset_root.join("images"))?;
    }

    on_status(0.80, "Đang cài: icons/zalo-gpui.png & zalo-gpui.desktop".to_owned());
    let apps_dir = data_home.join("applications");
    std::fs::create_dir_all(&apps_dir).map_err(|e| e.to_string())?;
    let desktop_file = apps_dir.join("zalo-gpui.desktop");
    let _ = std::fs::remove_file(&desktop_file);
    zalo_shell::install_app_entry_at(&data_home, zalo_shell::GPUI_APP_ID, &target_exe)
        .map_err(|e| format!("Ghi .desktop thất bại: {e}"))?;

    refresh_desktop_caches(&data_home, on_status);
    on_status(1.0, format!("Đã cài xong: {}", short_user_path(&target_exe)));
    Ok(target_exe)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("Tạo thư mục {}: {e}", dst.display()))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("Đọc thư mục {}: {e}", src.display()))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_file() {
            copy_file_atomic(&from, &to, false)?;
        }
    }
    Ok(())
}

fn refresh_desktop_caches(data_home: &Path, on_status: &mut dyn FnMut(f32, String)) {
    let apps = data_home.join("applications");
    on_status(0.88, "Đang chạy: update-desktop-database".to_owned());
    let _ = Command::new("update-desktop-database")
        .arg(&apps)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let icons = data_home.join("icons/hicolor");
    on_status(0.95, "Đang chạy: gtk-update-icon-cache".to_owned());
    let _ = Command::new("gtk-update-icon-cache")
        .args(["-f", "-t"])
        .arg(&icons)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Download `manifest.json` + `.tar.zst` from `ZALO_OCR_MANIFEST_URL` (or
/// [`OCR_MANIFEST_URL`]), verify SHA-256, extract into `$XDG_DATA_HOME/zalo-rust/ocr`,
/// and validate `zocr::install_in`.
pub(crate) fn download_and_install_ocr_r2<F>(mut on_event: F) -> Result<PathBuf, String>
where
    F: FnMut(InstallEvent),
{
    let dest = default_ocr_dir();
    let manifest_url = std::env::var("ZALO_OCR_MANIFEST_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| OCR_MANIFEST_URL.to_owned());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to create download runtime: {e}"))?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .user_agent("zalo-rust-installer/0.1")
        .build()
        .map_err(|e| format!("Failed to initialize HTTP client: {e}"))?;

    on_event(InstallEvent::Step {
        step: 1,
        progress: 0.02,
        status: "Đang tải: ocr-manifest.json".to_owned(),
    });
    let manifest: OcrManifest = rt.block_on(async {
        let resp = client
            .get(&manifest_url)
            .send()
            .await
            .map_err(|e| format!("Failed to reach OCR manifest endpoint: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("OCR manifest endpoint returned HTTP {}", resp.status()));
        }
        let body = resp
            .json::<Value>()
            .await
            .map_err(|e| format!("Invalid OCR manifest JSON: {e}"))?;
        OcrManifest::from_json(&body)
    })?;

    let arch = std::env::consts::ARCH;
    let expected_platform = format!("linux-{arch}");
    if manifest.platform != expected_platform {
        return Err(format!(
            "Gói OCR trên R2 là {}, chưa có bản cho kiến trúc {arch}",
            manifest.platform
        ));
    }

    let stage_root = dest
        .parent()
        .unwrap_or_else(|| Path::new("/tmp"))
        .to_path_buf();
    std::fs::create_dir_all(&stage_root).map_err(|e| e.to_string())?;
    let archive_part = stage_root.join("ocr-download.tar.zst.part");
    let staging_dir = stage_root.join("ocr.staging");
    let _ = std::fs::remove_file(&archive_part);
    let _ = std::fs::remove_dir_all(&staging_dir);

    let archive_name = manifest
        .url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("ocr-linux-x86_64.tar.zst")
        .to_owned();

    let actual_sha = rt.block_on(async {
        let mut resp = client
            .get(&manifest.url)
            .send()
            .await
            .map_err(|e| format!("Lỗi tải gói OCR: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Tải gói OCR thất bại (HTTP {})", resp.status()));
        }
        let total = resp.content_length().unwrap_or(manifest.size).max(1);
        let mut out = std::fs::File::create(&archive_part).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let mut done: u64 = 0;
        let started = Instant::now();
        let mut last_emit = Instant::now() - Duration::from_secs(1);

        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("Đứt kết nối khi tải OCR: {e}"))?
        {
            out.write_all(&chunk).map_err(|e| e.to_string())?;
            hasher.update(&chunk);
            done += chunk.len() as u64;
            if last_emit.elapsed() >= Duration::from_millis(60) || done >= total {
                let elapsed = started.elapsed().as_secs_f64().max(0.05);
                let speed = (done as f64 / elapsed) as u64;
                on_event(InstallEvent::OcrDownload {
                    archive: archive_name.clone(),
                    done_bytes: done,
                    total_bytes: total,
                    speed_bps: speed,
                });
                last_emit = Instant::now();
            }
        }
        out.flush().map_err(|e| e.to_string())?;
        Ok::<String, String>(hex::encode(hasher.finalize()))
    })?;

    if !actual_sha.eq_ignore_ascii_case(&manifest.sha256) {
        let _ = std::fs::remove_file(&archive_part);
        return Err(format!(
            "Sai mã kiểm tra SHA-256 (tải về {actual_sha}, mong đợi {})",
            manifest.sha256
        ));
    }
    on_event(InstallEvent::Step {
        step: 1,
        progress: 0.81,
        status: format!("Đang giải nén: {archive_name}"),
    });

    let mut extracted_idx = 0usize;
    extract_tar_zst_with_progress(&archive_part, &staging_dir, &mut |rel| {
        extracted_idx += 1;
        let pct = (0.82 + (extracted_idx.min(11) as f32) * 0.01).min(0.93);
        on_event(InstallEvent::Step {
            step: 1,
            progress: pct,
            status: format!("Đang giải nén: {rel}"),
        });
    })?;
    let _ = std::fs::remove_file(&archive_part);

    let install = zocr::install_in(&staging_dir)
        .ok_or_else(|| "Gói giải nén thiếu libonnxruntime.so hoặc models.json".to_owned())?;
    on_event(InstallEvent::Step {
        step: 1,
        progress: 0.95,
        status: "Đang chạy kiểm tra: libonnxruntime.so (zocr)".to_owned(),
    });
    zocr::Engine::open(&install.runtime, &install.manifest, zocr::Options::default())
        .map_err(|e| format!("Kiểm tra engine OCR thất bại: {e}"))?;

    let backup = stage_root.join("ocr.old");
    let _ = std::fs::remove_dir_all(&backup);
    if dest.exists() {
        std::fs::rename(&dest, &backup).map_err(|e| e.to_string())?;
    }
    if let Err(err) = std::fs::rename(&staging_dir, &dest) {
        if backup.exists() {
            let _ = std::fs::rename(&backup, &dest);
        }
        return Err(format!("Không ghi được vào {}: {err}", dest.display()));
    }
    let _ = std::fs::remove_dir_all(&backup);

    on_event(InstallEvent::Step {
        step: 1,
        progress: 1.0,
        status: format!("Đã cài xong: {}", short_user_path(&dest)),
    });
    Ok(dest)
}

#[cfg(test)]
fn extract_tar_zst(archive: &Path, dest: &Path) -> Result<(), String> {
    extract_tar_zst_with_progress(archive, dest, &mut |_| {})
}

/// Safe pure-Rust `.tar.zst` reader extracting regular files/dirs into `dest`.
fn extract_tar_zst_with_progress(
    archive: &Path,
    dest: &Path,
    on_entry: &mut dyn FnMut(&str),
) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut decoder =
        zstd::stream::read::Decoder::new(file).map_err(|e| format!("Lỗi giải nén zstd: {e}"))?;
    extract_ustar(&mut decoder, dest, on_entry)
}

fn extract_ustar(
    reader: &mut impl Read,
    dest: &Path,
    on_entry: &mut dyn FnMut(&str),
) -> Result<(), String> {
    let mut hdr = [0u8; 512];
    loop {
        if !read_exact_or_eof(reader, &mut hdr)? {
            break;
        }
        if hdr.iter().all(|&b| b == 0) {
            break;
        }
        let raw_name = c_str(&hdr[0..100]);
        let prefix = c_str(&hdr[345..500]);
        let full_rel = if prefix.is_empty() {
            raw_name
        } else {
            format!("{prefix}/{raw_name}")
        };
        let size = parse_tar_octal(&hdr[124..136])?;
        #[cfg(unix)]
        let mode = parse_tar_octal(&hdr[100..108]).unwrap_or(0o644) as u32;
        let typeflag = hdr[156];

        let target = sanitize_archive_path(dest, &full_rel)?;
        match typeflag {
            b'5' => {
                if let Some(dir) = target {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
            }
            b'0' | 0 => {
                on_entry(&full_rel);
                let out_path =
                    target.ok_or_else(|| format!("Đường dẫn file tar không hợp lệ: {full_rel}"))?;
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
                let mut limited = reader.take(size);
                io::copy(&mut limited, &mut out).map_err(|e| e.to_string())?;
                if limited.limit() > 0 {
                    return Err(format!("Archive bị cắt cụt khi đọc {full_rel}"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let file_mode = if mode & 0o111 != 0 || full_rel.ends_with(".so") {
                        0o755
                    } else {
                        0o644
                    };
                    let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(file_mode));
                }
                let rem = (size % 512) as usize;
                if rem != 0 {
                    let mut pad = [0u8; 512];
                    reader
                        .read_exact(&mut pad[..512 - rem])
                        .map_err(|e| format!("Thiếu padding tar sau {full_rel}: {e}"))?;
                }
            }
            b'x' | b'g' => {
                // Skip PAX extended header bytes + padding safely.
                let mut limited = reader.take(size);
                io::copy(&mut limited, &mut io::sink()).map_err(|e| e.to_string())?;
                let rem = (size % 512) as usize;
                if rem != 0 {
                    let mut pad = [0u8; 512];
                    reader.read_exact(&mut pad[..512 - rem]).map_err(|e| e.to_string())?;
                }
            }
            other => {
                return Err(format!(
                    "Loại mục tar không được hỗ trợ ({other}) cho {full_rel}"
                ));
            }
        }
    }
    Ok(())
}

fn read_exact_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool, String> {
    let mut read = 0;
    while read < buf.len() {
        match reader.read(&mut buf[read..]) {
            Ok(0) if read == 0 => return Ok(false),
            Ok(0) => return Err("Header tar không đủ 512 byte".to_owned()),
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(true)
}

fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn parse_tar_octal(bytes: &[u8]) -> Result<u64, String> {
    let s = c_str(bytes);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(trimmed, 8).map_err(|e| format!("Header tar sai số '{trimmed}': {e}"))
}

fn sanitize_archive_path(root: &Path, rel: &str) -> Result<Option<PathBuf>, String> {
    let p = Path::new(rel);
    let mut out = root.to_path_buf();
    let mut pushed = false;
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(seg) => {
                out.push(seg);
                pushed = true;
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("Đường dẫn tar không an toàn: {rel}"));
            }
        }
    }
    Ok(pushed.then_some(out))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WizardPhase {
    Ready,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepState {
    Pending,
    Active,
    Done,
    Failed,
}

/// Interactive GPUI installer window showing minimal Zalo file progress and,
/// once OCR download/install begins, the OCR progress bar underneath.
pub struct InstallerView {
    scan: SystemScan,
    phase: WizardPhase,
    install_state: StepState,
    install_pct: f32,
    install_status: String,
    ocr_visible: bool,
    ocr_state: StepState,
    ocr_pct: f32,
    ocr_status: String,
    error_line: Option<String>,
    installed_exe: Option<PathBuf>,
    rx: Option<Receiver<InstallEvent>>,
    polling: bool,
}

impl InstallerView {
    /// Construct a minimal installer view (no body logo or clutter).
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        window.set_window_title("Cài đặt Zalo Linux");
        window.activate_window();
        let scan = scan_system();
        let ready_status = format!("Sẵn sàng cài: {}", short_user_path(&user_bin_path()));
        let mut view = Self {
            scan,
            phase: WizardPhase::Ready,
            install_state: StepState::Pending,
            install_pct: 0.0,
            install_status: ready_status,
            ocr_visible: false,
            ocr_state: StepState::Pending,
            ocr_pct: 0.0,
            ocr_status: String::new(),
            error_line: None,
            installed_exe: None,
            rx: None,
            polling: false,
        };
        if std::env::args().any(|a| a == "--auto") {
            view.start_install(cx);
        }
        view
    }

    #[cfg(test)]
    fn new_for_test(scan: SystemScan) -> Self {
        Self {
            scan,
            phase: WizardPhase::Ready,
            install_state: StepState::Pending,
            install_pct: 0.0,
            install_status: "Sẵn sàng".to_owned(),
            ocr_visible: false,
            ocr_state: StepState::Pending,
            ocr_pct: 0.0,
            ocr_status: String::new(),
            error_line: None,
            installed_exe: None,
            rx: None,
            polling: false,
        }
    }

    fn apply_event(&mut self, ev: InstallEvent) -> bool {
        match ev {
            InstallEvent::Step {
                step,
                progress,
                status,
            } => {
                if step == 0 {
                    self.install_pct = progress.clamp(0.0, 1.0);
                    self.install_status = status;
                    self.install_state = if progress >= 1.0 {
                        StepState::Done
                    } else {
                        StepState::Active
                    };
                } else {
                    self.install_state = StepState::Done;
                    self.install_pct = 1.0;
                    self.ocr_visible = true;
                    self.ocr_pct = progress.clamp(0.0, 1.0);
                    self.ocr_status = status;
                    self.ocr_state = if progress >= 1.0 {
                        StepState::Done
                    } else {
                        StepState::Active
                    };
                }
                false
            }
            InstallEvent::OcrDownload {
                archive,
                done_bytes,
                total_bytes,
                speed_bps,
            } => {
                self.install_state = StepState::Done;
                self.install_pct = 1.0;
                self.ocr_visible = true;
                self.ocr_state = StepState::Active;
                let frac = (done_bytes as f64 / total_bytes.max(1) as f64).clamp(0.0, 1.0) as f32;
                self.ocr_pct = frac * 0.80;
                let done_mb = done_bytes as f64 / 1_048_576.0;
                let total_mb = total_bytes as f64 / 1_048_576.0;
                let speed_mb = speed_bps as f64 / 1_048_576.0;
                self.ocr_status = format!(
                    "Đang tải: {archive} ({done_mb:.1} / {total_mb:.1} MB · {:.0}% · {speed_mb:.1} MB/s)",
                    frac * 100.0
                );
                false
            }
            InstallEvent::Log(_) => false,
            InstallEvent::Done { exe_path } => {
                self.install_state = StepState::Done;
                self.install_pct = 1.0;
                self.ocr_visible = true;
                self.ocr_state = StepState::Done;
                self.ocr_pct = 1.0;
                self.phase = WizardPhase::Done;
                self.installed_exe = Some(exe_path);
                true
            }
            InstallEvent::Error(err) => {
                if self.install_state == StepState::Active {
                    self.install_state = StepState::Failed;
                    self.install_status = format!("Lỗi: {err}");
                }
                if self.ocr_state == StepState::Active {
                    self.ocr_state = StepState::Failed;
                    self.ocr_status = format!("Lỗi: {err}");
                }
                self.phase = WizardPhase::Failed;
                self.error_line = Some(err);
                true
            }
        }
    }

    fn start_install(&mut self, cx: &mut Context<Self>) {
        if self.phase == WizardPhase::Running {
            return;
        }
        let scan = self.scan.clone();
        self.phase = WizardPhase::Running;
        self.install_state = StepState::Active;
        self.install_pct = 0.05;
        self.install_status = "Đang kiểm tra thư viện hệ thống...".to_owned();
        self.ocr_visible = false;
        self.ocr_state = StepState::Pending;
        self.ocr_pct = 0.0;
        self.ocr_status.clear();
        self.error_line = None;
        self.installed_exe = None;

        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);

        std::thread::Builder::new()
            .name("zalo-installer-worker".into())
            .spawn(move || {
                let emit = |ev: InstallEvent| {
                    let _ = tx.send(ev);
                };

                let mut on_zalo = |progress: f32, status: String| {
                    emit(InstallEvent::Step {
                        step: 0,
                        progress,
                        status,
                    });
                };

                if let Err(e) = ensure_missing_required_deps(&scan, &mut on_zalo) {
                    emit(InstallEvent::Error(e));
                    return;
                }
                let src_exe = match current_source_exe() {
                    Ok(p) => p,
                    Err(e) => {
                        emit(InstallEvent::Error(e));
                        return;
                    }
                };
                let exe_path = match install_user_local(&src_exe, &mut on_zalo) {
                    Ok(p) => p,
                    Err(e) => {
                        emit(InstallEvent::Error(e));
                        return;
                    }
                };

                if let Err(e) = download_and_install_ocr_r2(&emit) {
                    emit(InstallEvent::Error(e));
                    return;
                }

                emit(InstallEvent::Done { exe_path });
            })
            .expect("spawn installer thread");

        self.ensure_poll(cx);
    }

    fn ensure_poll(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| loop {
            gpui::Timer::after(Duration::from_millis(60)).await;
            let mut keep_running = false;
            let ok = this.update(cx, |view, cx| {
                keep_running = view.drain_events();
                cx.notify();
            });
            if ok.is_err() || !keep_running {
                let _ = this.update(cx, |view, _| {
                    view.polling = false;
                });
                break;
            }
        })
        .detach();
    }

    fn drain_events(&mut self) -> bool {
        let Some(rx) = self.rx.take() else {
            return false;
        };
        let mut finished = false;
        while let Ok(ev) = rx.try_recv() {
            if self.apply_event(ev) {
                finished = true;
            }
        }
        if !finished {
            self.rx = Some(rx);
            return self.phase == WizardPhase::Running;
        }
        false
    }

    fn launch_installed(&self, window: &mut Window, cx: &mut Context<Self>) {
        let exe = self
            .installed_exe
            .clone()
            .or_else(|| self.scan.existing_binary.clone())
            .unwrap_or_else(user_bin_path);
        if exe.is_file() {
            let _ = Command::new(&exe)
                .arg("--show")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
        window.remove_window();
        cx.quit();
    }
}

impl Render for InstallerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let blue = token_rgb("--B55", "#0068ff");
        let blue_hover = token_rgb("--B60", "#005ae0");
        let card_bg = token_rgb("--layer-background-subtle", "#f4f6f9");
        let red = token_rgb("--text-errors", "#c31818");

        let corners = CsdCorners::from_window(window);
        let running = self.phase == WizardPhase::Running;
        let done = self.phase == WizardPhase::Done;
        let failed = self.phase == WizardPhase::Failed;

        wrap_csd(
            window,
            corners
                .apply(
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .overflow_hidden()
                        .bg(rgb(token_rgb("--layer-background", "#ffffff"))),
                    true,
                    true,
                    true,
                    true,
                )
                .text_color(rgb(ink))
                .font_family(crate::fonts::FAMILY_UI)
                .child(crate::titlebar::installer_titlebar(
                    corners,
                    "Cài đặt Zalo Linux".to_owned(),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_h(px(0.))
                        .px(px(20.))
                        .pt(px(16.))
                        .pb(px(8.))
                        .flex()
                        .flex_col()
                        .justify_center()
                        .gap(px(12.))
                        .child(progress_section(
                            &self.install_status,
                            self.install_pct,
                            self.install_state == StepState::Failed,
                        ))
                        .when(self.ocr_visible, |el| {
                            el.child(progress_section(
                                &self.ocr_status,
                                self.ocr_pct,
                                self.ocr_state == StepState::Failed,
                            ))
                        })
                        .when_some(self.error_line.clone(), |el, err| {
                            el.when(
                                !self.install_status.contains(&err)
                                    && !self.ocr_status.contains(&err),
                                |el| {
                                    el.child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(rgb(red))
                                            .truncate()
                                            .child(err),
                                    )
                                },
                            )
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .px(px(20.))
                        .pb(px(14.))
                        .pt(px(4.))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            div()
                                .id("installer-close")
                                .px(px(14.))
                                .h(px(30.))
                                .rounded(px(4.))
                                .flex()
                                .items_center()
                                .bg(rgb(card_bg))
                                .text_size(px(12.5))
                                .text_color(rgb(ink))
                                .cursor_pointer()
                                .on_click(cx.listener(|_, _, window, cx| {
                                    window.remove_window();
                                    cx.quit();
                                }))
                                .child(if done || failed { "Đóng" } else { "Hủy" }),
                        )
                        .when(!running, |el| {
                            el.child(
                                div()
                                    .id("installer-primary")
                                    .px(px(16.))
                                    .h(px(30.))
                                    .rounded(px(4.))
                                    .flex()
                                    .items_center()
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_size(px(12.5))
                                    .text_color(rgb(crate::color::on_accent_fg()))
                                    .bg(rgb(blue))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(blue_hover)))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if this.phase == WizardPhase::Done {
                                            this.launch_installed(window, cx);
                                        } else {
                                            this.start_install(cx);
                                        }
                                        cx.notify();
                                    }))
                                    .child(if done {
                                        "Mở Zalo"
                                    } else if failed {
                                        "Thử lại"
                                    } else {
                                        "Bắt đầu"
                                    }),
                            )
                        }),
                ),
        )
    }
}

fn progress_section(status: &str, progress: f32, failed: bool) -> gpui::Div {
    let ink = token_rgb("--text-primary", "#081b3a");
    let red = token_rgb("--text-errors", "#c31818");
    let green = 0x16_a3_4a;
    let track = token_rgb("--layer-background-hover", "#e5e7eb");

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(
            div()
                .w_full()
                .text_size(px(12.5))
                .text_color(rgb(if failed { red } else { ink }))
                .truncate()
                .child(status.to_owned()),
        )
        .child(
            div()
                .w_full()
                .h(px(16.))
                .rounded(px(3.))
                .border_1()
                .border_color(crate::color::border_subtle())
                .overflow_hidden()
                .bg(rgb(track))
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(progress.clamp(0.0, 1.0)))
                        .bg(rgb(if failed { red } else { green })),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_os_release_and_classifies_distros() {
        let deb = "PRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nID=debian\n";
        let m = parse_os_release(deb);
        assert_eq!(m.get("ID").map(String::as_str), Some("debian"));
        assert_eq!(classify_distro("debian", ""), DistroFamily::Debian);
        assert_eq!(classify_distro("linuxmint", "ubuntu debian"), DistroFamily::Debian);
        assert_eq!(classify_distro("nobara", "fedora"), DistroFamily::Fedora);
        assert_eq!(classify_distro("cachyos", "arch"), DistroFamily::Arch);
        assert_eq!(classify_distro("opensuse-tumbleweed", "opensuse"), DistroFamily::OpenSuse);
        assert_eq!(classify_distro("nixos", ""), DistroFamily::Generic);
    }

    #[test]
    fn sanitize_rejects_traversal_and_absolute_paths() {
        let root = Path::new("/tmp/ocr");
        assert!(sanitize_archive_path(root, "../evil").is_err());
        assert!(sanitize_archive_path(root, "/etc/passwd").is_err());
        assert_eq!(
            sanitize_archive_path(root, "zocr/models.json").expect("safe"),
            Some(PathBuf::from("/tmp/ocr/zocr/models.json"))
        );
    }

    #[test]
    fn extract_tar_zst_unpacks_runtime_and_models() {
        let root = std::env::temp_dir().join(format!("zalo-ocr-tar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");

        // Build a minimal ustar tar in memory with libonnxruntime.so and zocr/models.json.
        let mut tar_bytes = Vec::new();
        for (name, body) in [
            ("libonnxruntime.so", b"ELF" as &[u8]),
            ("zocr/models.json", b"{}" as &[u8]),
        ] {
            let mut hdr = [0u8; 512];
            hdr[..name.len()].copy_from_slice(name.as_bytes());
            hdr[100..107].copy_from_slice(b"0000755");
            let sz = format!("{:011o}", body.len());
            hdr[124..135].copy_from_slice(sz.as_bytes());
            hdr[156] = b'0';
            hdr[257..263].copy_from_slice(b"ustar\0");
            hdr[263..265].copy_from_slice(b"00");
            // Compute ustar checksum (bytes 148..156 treated as spaces).
            hdr[148..156].fill(b' ');
            let sum: u32 = hdr.iter().map(|&b| u32::from(b)).sum();
            let cksum = format!("{sum:06o}\0 ");
            hdr[148..156].copy_from_slice(cksum.as_bytes());
            tar_bytes.extend_from_slice(&hdr);
            tar_bytes.extend_from_slice(body);
            let rem = body.len() % 512;
            if rem != 0 {
                tar_bytes.resize(tar_bytes.len() + (512 - rem), 0);
            }
        }
        tar_bytes.resize(tar_bytes.len() + 1024, 0);
        let compressed = zstd::bulk::compress(&tar_bytes, 3).expect("zstd");
        let archive = root.join("ocr.tar.zst");
        let out_dir = root.join("out");
        std::fs::write(&archive, compressed).expect("write");

        extract_tar_zst(&archive, &out_dir).expect("extract");
        assert!(zocr::install_in(&out_dir).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "downloads live OCR pack from configured OCR manifest URL"]
    fn live_r2_ocr_download_and_engine_open() {
        let mut got_progress = false;
        let dir = download_and_install_ocr_r2(|ev| {
            if matches!(ev, InstallEvent::OcrDownload { .. }) {
                got_progress = true;
            }
        })
        .expect("live R2 OCR download & extract");
        assert!(got_progress);
        assert!(zocr::install_in(&dir).is_some());
    }

    #[test]
    fn ocr_bar_remains_hidden_until_ocr_step_begins() {
        let mut view = InstallerView::new_for_test(scan_system());
        assert!(!view.ocr_visible);

        assert!(!view.apply_event(InstallEvent::Step {
            step: 0,
            progress: 0.4,
            status: "Đang cài: fonts/Zalo-Segoe-UI.ttf".to_owned(),
        }));
        assert!(!view.ocr_visible);
        assert_eq!(view.install_status, "Đang cài: fonts/Zalo-Segoe-UI.ttf");

        assert!(!view.apply_event(InstallEvent::OcrDownload {
            archive: "ocr-linux-x86_64-v1.tar.zst".to_owned(),
            done_bytes: 5_242_880,
            total_bytes: 10_485_760,
            speed_bps: 2_097_152,
        }));
        assert!(view.ocr_visible);
        assert!((view.install_pct - 1.0).abs() < f32::EPSILON);
        assert!(view.ocr_status.contains("ocr-linux-x86_64-v1.tar.zst"));
    }
}
