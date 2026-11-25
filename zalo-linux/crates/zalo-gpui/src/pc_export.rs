//! PC → PC copy of this client's account directory.
//!
//! Electron has no server protocol that moves a PC account onto another PC.
//! This archive is one uncompressed zip of the account files on this machine
//! (`session.json`, `device.json`, `store.db`, profiles, cursors) plus the
//! account settings kept beside that directory. Official Zalo PC cannot import
//! the file, and this client refuses a zip that is not its own export.
//! Version 1 was `store.db` only and still imports.

use std::path::{Path, PathBuf};

use gpui::{
    div, prelude::*, px, rgb, Context, InteractiveElement, ParentElement, StatefulInteractiveElement,
    Styled,
};

use crate::color::{str_vi, token_rgb};
use crate::fonts::FAMILY_SEMIBOLD;
use crate::shell::ShellView;

/// Sheet the Xuất / Nhập card is showing. This is a local zip, not cmds 630–634.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TransferPopup {
    /// No dialog.
    Closed,
    /// Confirm replacing this machine's account.
    ImportAsk,
    /// Export form. The folder picker opens only from Thay đổi.
    ExportForm,
    /// Import form. The file picker opens only from Chọn File.
    ImportForm,
}

/// Xuất / Nhập form state. The PC-to-phone backup engine does not live here.
#[derive(Clone, Debug, Default)]
pub(crate) struct TransferUi {
    /// Dialog.
    pub popup: TransferPopup,
    /// A zip copy is running.
    pub busy: bool,
    /// Zip chosen for import, waiting for confirm.
    pub import_path: Option<PathBuf>,
    /// Folder shown on the export form. Empty until the form opens.
    pub export_dir: Option<PathBuf>,
    /// Zip name frozen when the export form opens.
    pub export_name: Option<String>,
}

impl Default for TransferPopup {
    fn default() -> Self {
        Self::Closed
    }
}

impl TransferUi {
    pub(crate) fn popup_open(&self) -> bool {
        self.popup != TransferPopup::Closed
    }
}

#[derive(Clone, Copy, Debug)]
enum TransferClick {
    Later,
    ImportYes,
    ImportNo,
    ExportChange,
    ExportRun,
    ImportChoose,
    ImportRun,
    ShowExport,
    ShowImport,
    OpenInstallDir,
    OpenZaloDir,
}

const KIND: &str = "zalo-rust-pc-export";
const VERSION: u32 = 2;
const MANIFEST: &str = "manifest.json";
const DB_NAME: &str = "store.db";
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

/// Account files copied into the zip. Sidecars and in-progress backup files stay out.
const ACCOUNT_FILES: &[&str] = &[
    "session.json",
    "device.json",
    "account.uid",
    "queue-cursors.json",
    "profiles.json",
    DB_NAME,
];

/// Prefs live in the machine config directory, not inside `accounts/<id>`.
const PREF_FILES: &[(&str, &str)] = &[
    ("ui-state.json", "prefs-ui-state.json"),
    ("gpui-prefs.json", "prefs-gpui.json"),
];

/// Writes the account in `dir` into `dest` as an uncompressed zip.
///
/// # Errors
///
/// Returns a message when the account is missing or the folder cannot be written.
pub fn export_store(dir: &Path, dest: &Path) -> Result<(), String> {
    let db_path = dir.join(DB_NAME);
    if db_path.is_file() {
        let _ = zalo_storage::LocalStore::checkpoint(&db_path);
    }
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for name in ACCOUNT_FILES {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(io_msg)?;
        if *name == DB_NAME && !bytes.starts_with(SQLITE_MAGIC) {
            return Err("store.db không phải cơ sở dữ liệu của client này.".into());
        }
        files.push(((*name).to_owned(), bytes));
    }
    if files.is_empty() {
        return Err("Chưa có dữ liệu tài khoản trên máy này để xuất.".into());
    }
    if let Some(machine) = machine_config(dir) {
        for (disk_name, entry_name) in PREF_FILES {
            let path = machine.join(disk_name);
            if path.is_file() {
                if let Ok(bytes) = std::fs::read(path) {
                    files.push(((*entry_name).to_owned(), bytes));
                }
            }
        }
    }
    let manifest = format!(r#"{{"kind":"{KIND}","version":{VERSION}}}"#);
    let mut parts: Vec<(&str, &[u8])> = Vec::with_capacity(files.len() + 1);
    parts.push((MANIFEST, manifest.as_bytes()));
    for (name, data) in &files {
        parts.push((name.as_str(), data.as_slice()));
    }
    let zip = zip_store(&parts);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io_msg)?;
    }
    std::fs::write(dest, &zip).map_err(io_msg)?;
    set_private(dest);
    Ok(())
}

/// Replaces the account in `dir` with a zip this client wrote.
///
/// Version 1 only replaces `store.db`. Version 2 also writes the account files
/// and, when `dir` sits in `accounts/`, the device file and prefs on the
/// machine config directory. The previous database is kept as `store.db.prev`.
///
/// # Errors
///
/// Returns a message when the zip is not this client's export.
pub fn import_store(dir: &Path, zip_path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(zip_path).map_err(io_msg)?;
    let entries = unzip_store(&bytes)?;
    let version = package_version(&entries)?;
    if version == 1 {
        let db = entry_bytes(&entries, DB_NAME).ok_or_else(foreign_package)?;
        if !db.starts_with(SQLITE_MAGIC) {
            return Err(foreign_package());
        }
        return replace_store(dir, db);
    }
    if version != 2 {
        return Err(foreign_package());
    }
    let db = entry_bytes(&entries, DB_NAME);
    let session = entry_bytes(&entries, "session.json");
    if db.is_none() && session.is_none() {
        return Err(foreign_package());
    }
    if let Some(db) = db {
        if !db.starts_with(SQLITE_MAGIC) {
            return Err(foreign_package());
        }
        replace_store(dir, db)?;
    }
    std::fs::create_dir_all(dir).map_err(io_msg)?;
    for (name, data) in &entries {
        if name == MANIFEST || name == DB_NAME || PREF_FILES.iter().any(|(_, entry)| entry == name) {
            continue;
        }
        if !ACCOUNT_FILES.contains(&name.as_str()) {
            continue;
        }
        write_account_file(&dir.join(name), data)?;
    }
    if let Some(machine) = machine_config(dir) {
        std::fs::create_dir_all(&machine).map_err(io_msg)?;
        if let Some(data) = entry_bytes(&entries, "device.json") {
            write_account_file(&machine.join("device.json"), data)?;
        }
        for (disk_name, entry_name) in PREF_FILES {
            if let Some(data) = entry_bytes(&entries, entry_name) {
                std::fs::write(machine.join(disk_name), data).map_err(io_msg)?;
            }
        }
    }
    Ok(())
}

/// `userId` stored in the zip, when the archive has one.
#[must_use]
pub fn packaged_user_id(zip_path: &Path) -> Option<String> {
    let bytes = std::fs::read(zip_path).ok()?;
    let entries = unzip_store(&bytes).ok()?;
    user_id_of(&entries)
}

/// Data directory of this client, when the folder already exists.
#[must_use]
pub fn installed_zalo_dir() -> Option<PathBuf> {
    let dir = crate::data::config_dir()?;
    dir.is_dir().then_some(dir)
}

/// Folder of the binary that is running, or the user-local install when this
/// process cannot see its own executable.
#[must_use]
pub fn zalo_install_dir() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.is_dir() {
                return Some(dir.to_path_buf());
            }
        }
    }
    crate::installer::installed_bin_dir()
}

/// One blue path on the export / import sheet. Windows Zalo shows the install
/// folder and, when it differs, the folder that holds the account data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZaloPlace {
    /// Row label.
    pub label: &'static str,
    /// Folder to open.
    pub path: PathBuf,
    /// `true` for the data directory, `false` for the install directory.
    pub data: bool,
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(left), Ok(right)) => left == right,
        _ => a == b,
    }
}

/// Install row, plus the data row when that folder is a different path.
#[must_use]
pub fn zalo_places(install: Option<PathBuf>, data: Option<PathBuf>) -> Vec<ZaloPlace> {
    match (install, data) {
        (Some(install), Some(data)) if same_dir(&install, &data) => {
            vec![ZaloPlace {
                label: "Nơi cài",
                path: install,
                data: false,
            }]
        }
        (Some(install), Some(data)) => vec![
            ZaloPlace {
                label: "Nơi cài",
                path: install,
                data: false,
            },
            ZaloPlace {
                label: "Nơi chứa dữ liệu",
                path: data,
                data: true,
            },
        ],
        (Some(install), None) => vec![ZaloPlace {
            label: "Nơi cài",
            path: install,
            data: false,
        }],
        (None, Some(data)) => vec![ZaloPlace {
            label: "Nơi chứa dữ liệu",
            path: data,
            data: true,
        }],
        (None, None) => Vec::new(),
    }
}

/// File name frozen for one export form.
///
/// `ddmmyyyy-hhmmss-tenpc-zalo-tenhedieuhanh.zip` in Vietnam time.
/// `computer_name` comes from `device.json` in `dir`. The OS token is
/// `/etc/os-release` `ID`, or the Rust OS name when that file is missing.
#[must_use]
pub fn export_file_name_for(dir: &Path) -> String {
    export_stamp_name(crate::data::now_ms(), &computer_token(dir), &os_token())
}

fn export_stamp_name(ms: i64, computer: &str, os_name: &str) -> String {
    let (year, month, day) = crate::data::vn_ymd(ms);
    let tod = (ms + 7 * 3_600_000).rem_euclid(86_400_000);
    let hour = tod / 3_600_000;
    let minute = (tod % 3_600_000) / 60_000;
    let second = (tod % 60_000) / 1_000;
    format!(
        "{day:02}{month:02}{year:04}-{hour:02}{minute:02}{second:02}-{}-zalo-{}.zip",
        file_token(computer),
        file_token(os_name),
    )
}

fn file_token(raw: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "pc".into()
    } else {
        out
    }
}

fn computer_token(dir: &Path) -> String {
    if let Ok(text) = std::fs::read_to_string(dir.join("device.json")) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(name) = value.get("computer_name").and_then(|item| item.as_str()) {
                let token = file_token(name);
                if token != "pc" {
                    return token;
                }
            }
        }
    }
    if let Ok(name) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let token = file_token(name.trim());
        if token != "pc" {
            return token;
        }
    }
    "pc".into()
}

fn os_token() -> String {
    if let Ok(text) = std::fs::read_to_string("/etc/os-release") {
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("ID=") else {
                continue;
            };
            let token = file_token(rest.trim().trim_matches('"'));
            if token != "pc" {
                return token;
            }
        }
    }
    file_token(std::env::consts::OS)
}

pub(crate) fn safe_export_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".zip") else {
        return false;
    };
    !stem.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
}

fn foreign_package() -> String {
    "File này không phải gói xuất của client này. Zalo PC chính thức dùng cơ sở dữ liệu riêng; máy này chỉ nhập gói do chính nó xuất.".into()
}

fn io_msg(err: std::io::Error) -> String {
    if err.raw_os_error() == Some(28) {
        "Ổ cứng đầy. Vui lòng dọn dung lượng để tiếp tục.".into()
    } else {
        err.to_string()
    }
}

fn machine_config(dir: &Path) -> Option<PathBuf> {
    if dir
        .parent()
        .and_then(|parent| parent.file_name())
        .is_some_and(|name| name == "accounts")
    {
        return dir.parent().and_then(|parent| parent.parent()).map(Path::to_path_buf);
    }
    None
}

fn entry_bytes<'a>(entries: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    entries
        .iter()
        .find(|(entry, _)| entry == name)
        .map(|(_, data)| data.as_slice())
}

fn package_version(entries: &[(String, Vec<u8>)]) -> Result<u32, String> {
    let manifest = entry_bytes(entries, MANIFEST).ok_or_else(foreign_package)?;
    let text = String::from_utf8_lossy(manifest);
    if !text.contains(&format!("\"kind\":\"{KIND}\"")) {
        return Err(foreign_package());
    }
    manifest_version(&text).ok_or_else(foreign_package)
}

fn manifest_version(text: &str) -> Option<u32> {
    let rest = text.split("\"version\":").nth(1)?;
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

fn user_id_of(entries: &[(String, Vec<u8>)]) -> Option<String> {
    if let Some(data) = entry_bytes(entries, "account.uid") {
        let id = String::from_utf8_lossy(data).trim().to_owned();
        if !id.is_empty() {
            return Some(id);
        }
    }
    let data = entry_bytes(entries, "session.json")?;
    let value: serde_json::Value = serde_json::from_slice(data).ok()?;
    let id = value.get("user_id").and_then(|item| item.as_str()).unwrap_or("").trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_owned())
    }
}

fn replace_store(dir: &Path, db: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(io_msg)?;
    let incoming = dir.join("store.db.incoming");
    std::fs::write(&incoming, db).map_err(io_msg)?;
    let current = dir.join(DB_NAME);
    if current.is_file() {
        let _ = zalo_storage::LocalStore::checkpoint(&current);
        let prev = dir.join("store.db.prev");
        let _ = std::fs::remove_file(&prev);
        std::fs::rename(&current, &prev).map_err(io_msg)?;
    }
    std::fs::rename(&incoming, &current).map_err(io_msg)?;
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(dir.join(format!("store.db{suffix}")));
    }
    Ok(())
}

fn write_account_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_msg)?;
    }
    std::fs::write(path, bytes).map_err(io_msg)?;
    set_private(path);
    Ok(())
}

fn set_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn zip_store(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut local = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let name_b = name.as_bytes();
        let crc = crc32(data);
        let local_at = u32::try_from(local.len()).unwrap_or(0);
        local.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        local.extend_from_slice(&20u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&crc.to_le_bytes());
        let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
        local.extend_from_slice(&size.to_le_bytes());
        local.extend_from_slice(&size.to_le_bytes());
        let name_len = u16::try_from(name_b.len()).unwrap_or(0);
        local.extend_from_slice(&name_len.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(name_b);
        local.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&local_at.to_le_bytes());
        central.extend_from_slice(name_b);
    }
    let mut out = local;
    let central_at = u32::try_from(out.len()).unwrap_or(0);
    let central_len = u32::try_from(central.len()).unwrap_or(0);
    out.extend_from_slice(&central);
    let count = u16::try_from(files.len()).unwrap_or(0);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_len.to_le_bytes());
    out.extend_from_slice(&central_at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn unzip_store(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 4 <= bytes.len() {
        let sig = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or([0; 4]));
        if sig == 0x0201_4b50 || sig == 0x0605_4b50 {
            break;
        }
        if sig != 0x0403_4b50 || at + 30 > bytes.len() {
            return Err("Gói zip không đọc được.".into());
        }
        let method = u16::from_le_bytes([bytes[at + 8], bytes[at + 9]]);
        if method != 0 {
            return Err("Gói zip nén khác định dạng client này.".into());
        }
        let crc = u32::from_le_bytes(bytes[at + 14..at + 18].try_into().unwrap_or([0; 4]));
        let size = u32::from_le_bytes(bytes[at + 18..at + 22].try_into().unwrap_or([0; 4])) as usize;
        let name_len = u16::from_le_bytes([bytes[at + 26], bytes[at + 27]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[at + 28], bytes[at + 29]]) as usize;
        let name_at = at + 30;
        let data_at = name_at + name_len + extra_len;
        let data_end = data_at + size;
        if data_end > bytes.len() || name_at + name_len > bytes.len() {
            return Err("Gói zip bị cắt.".into());
        }
        let name = String::from_utf8_lossy(&bytes[name_at..name_at + name_len]).into_owned();
        if name.contains("..") || name.contains('/') || name.contains('\\') {
            return Err("Gói zip có đường dẫn không hợp lệ.".into());
        }
        let data = bytes[data_at..data_end].to_vec();
        if crc32(&data) != crc {
            return Err("Gói zip sai checksum.".into());
        }
        out.push((name, data));
        at = data_end;
    }
    if out.is_empty() {
        return Err("Gói zip không có tệp.".into());
    }
    Ok(out)
}

fn home_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |path: PathBuf| {
        if path.as_os_str().is_empty() || out.iter().any(|have| have == &path) {
            return;
        }
        if path.is_dir() {
            out.push(path);
        }
    };
    if let Some(home) = std::env::var_os("HOME") {
        push(PathBuf::from(home));
    }
    // The process HOME can be a profile folder with no Downloads.
    // The login account still has the real one.
    if let Some(user) = std::env::var_os("USER").filter(|user| !user.is_empty()) {
        push(PathBuf::from("/home").join(user));
    }
    out
}

fn xdg_download(home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).ok()?;
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("XDG_DOWNLOAD_DIR=") else {
            continue;
        };
        let raw = rest.trim().trim_matches('"');
        let expanded = raw.replace("$HOME", &home.to_string_lossy());
        let path = PathBuf::from(expanded);
        if path.is_dir() {
            return Some(path);
        }
    }
    None
}

fn default_export_dir() -> PathBuf {
    for home in home_candidates() {
        if let Some(dir) = xdg_download(&home) {
            return dir;
        }
        for name in ["Downloads", "Tải xuống", "Download"] {
            let dir = home.join(name);
            if dir.is_dir() {
                return dir;
            }
        }
    }
    home_candidates()
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("."))
}

impl ShellView {
    /// Opens the in-app export form. The folder picker stays on Thay đổi.
    pub(crate) fn open_pc_export(&mut self, cx: &mut Context<Self>) {
        if self.blocked_transfer_note() {
            return;
        }
        if self.session_dir.is_none() || !self.has_session {
            self.set_note("Cần đăng nhập để xuất dữ liệu.");
            return;
        }
        if self
            .export_ui
            .export_dir
            .as_ref()
            .is_none_or(|dir| !dir.is_dir())
        {
            self.export_ui.export_dir = Some(default_export_dir());
        }
        self.ensure_export_name(true);
        self.export_ui.popup = TransferPopup::ExportForm;
        cx.notify();
    }

    /// Switches the open transfer dialog to the export tab.
    pub(crate) fn show_pc_export_tab(&mut self) {
        if self
            .export_ui
            .export_dir
            .as_ref()
            .is_none_or(|dir| !dir.is_dir())
        {
            self.export_ui.export_dir = Some(default_export_dir());
        }
        self.ensure_export_name(false);
        self.export_ui.popup = TransferPopup::ExportForm;
    }

    fn ensure_export_name(&mut self, fresh: bool) {
        if !fresh && self.export_ui.export_name.as_ref().is_some_and(|name| safe_export_name(name)) {
            return;
        }
        let dir = self.session_dir.clone().unwrap_or_else(|| PathBuf::from("."));
        self.export_ui.export_name = Some(export_file_name_for(&dir));
    }

    /// Opens the folder of the running binary.
    pub(crate) fn open_install_folder(&mut self) {
        let Some(dir) = zalo_install_dir() else {
            self.set_note("Không thấy thư mục cài Zalo.");
            return;
        };
        self.reveal_dir(&dir);
    }

    /// Opens this client's data folder when it is already on the machine.
    pub(crate) fn open_zalo_data_folder(&mut self) {
        let Some(dir) = installed_zalo_dir() else {
            self.set_note("Máy này chưa cài Zalo.");
            return;
        };
        self.reveal_dir(&dir);
    }

    fn reveal_dir(&mut self, dir: &Path) {
        match crate::os_util::open_external(dir) {
            Ok(()) => {}
            Err(err) => self.set_note(err.to_string()),
        }
    }

    /// Switches the open transfer dialog to the import tab.
    pub(crate) fn show_pc_import_tab(&mut self) {
        self.export_ui.popup = TransferPopup::ImportForm;
    }

    /// Opens the in-app import form. The file picker stays on Chọn File.
    pub(crate) fn open_pc_import(&mut self, cx: &mut Context<Self>) {
        if self.blocked_transfer_note() {
            return;
        }
        if self.session_dir.is_none() || !self.has_session {
            self.set_note("Cần đăng nhập để nhập dữ liệu.");
            return;
        }
        self.export_ui.popup = TransferPopup::ImportForm;
        cx.notify();
    }

    /// Thay đổi on the export form. Cancel leaves the shown folder as it is.
    pub(crate) fn change_export_folder(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async { zalo_shell::pick_folder("Chọn thư mục lưu dữ liệu xuất") })
                .await;
            let _ = this.update(cx, |view, cx| {
                match picked {
                    Ok(Some(folder)) => view.export_ui.export_dir = Some(folder),
                    Ok(None) => {}
                    Err(err) => view.set_note(err.to_string()),
                }
                if view.export_ui.popup == TransferPopup::Closed {
                    view.export_ui.popup = TransferPopup::ExportForm;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Chọn File on the import form. Cancel leaves the form open.
    pub(crate) fn choose_import_file(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async { zalo_shell::pick_file("Chọn gói xuất dữ liệu") })
                .await;
            let _ = this.update(cx, |view, cx| {
                match picked {
                    Ok(Some(path)) => view.export_ui.import_path = Some(path),
                    Ok(None) => {}
                    Err(err) => view.set_note(err.to_string()),
                }
                view.export_ui.popup = TransferPopup::ImportForm;
                cx.notify();
            });
        })
        .detach();
    }

    /// Writes the zip into the folder already shown on the export form.
    pub(crate) fn commit_pc_export(&mut self, cx: &mut Context<Self>) {
        if self.blocked_transfer_note() {
            return;
        }
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            self.set_note("Cần đăng nhập để xuất dữ liệu.");
            self.export_ui.popup = TransferPopup::Closed;
            return;
        };
        let Some(folder) = self.export_ui.export_dir.clone().filter(|path| path.is_dir()) else {
            self.set_note("Chưa chọn thư mục lưu.");
            return;
        };
        let name = self
            .export_ui
            .export_name
            .clone()
            .filter(|item| safe_export_name(item))
            .unwrap_or_else(|| export_file_name_for(&dir));
        let dest = folder.join(name);
        self.export_ui.popup = TransferPopup::Closed;
        self.export_ui.busy = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { export_store(&dir, &dest).map(|()| dest) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.export_ui.busy = false;
                match result {
                    Ok(dest) => view.set_note(format!(
                        "{} {}",
                        str_vi(
                            "STR_EXPORT_SUCCESS_MESSAGE",
                            "Xuất dữ liệu thành công, bạn có thể nhập dữ liệu tại:"
                        ),
                        dest.display()
                    )),
                    Err(err) => view.set_note(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn finish_pc_import(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.export_ui.import_path.clone() else {
            self.export_ui.popup = TransferPopup::Closed;
            return;
        };
        let Some(dir) = self.session_dir.clone().or_else(crate::data::config_dir) else {
            self.set_note("Máy này chưa cài Zalo. Cần cài Zalo trước khi nhập dữ liệu.");
            return;
        };
        if installed_zalo_dir().is_none() {
            self.set_note("Máy này chưa cài Zalo. Cần cài Zalo trước khi nhập dữ liệu.");
            return;
        }
        let dest = import_destination(&path, &dir);
        self.export_ui.popup = TransferPopup::Closed;
        self.export_ui.import_path = None;
        self.export_ui.busy = true;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn({
                    let dest = dest.clone();
                    let path = path.clone();
                    async move { import_store(&dest, &path) }
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.export_ui.busy = false;
                match res {
                    Ok(()) => {
                        if let Some(uid) = packaged_user_id(&path) {
                            if let Some(machine) = crate::data::config_dir() {
                                crate::data::write_current_user(&machine, &uid);
                            }
                        }
                        view.session_dir = Some(dest);
                        view.apply_imported_snapshot();
                        view.set_note("Đã nhập dữ liệu. Tài khoản trên máy này dùng bản sao lưu vừa chọn.");
                    }
                    Err(err) => view.set_note(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn blocked_transfer_note(&mut self) -> bool {
        if self.export_ui.busy {
            self.set_note(
                str_vi(
                    "STR_BACKUP_MSG_ACTION_BLOCKED_BY_EXPORT_IMPORT",
                    "Không thể thực hiện do đang nhập hoặc xuất dữ liệu",
                )
                .to_owned(),
            );
            true
        } else {
            false
        }
    }

    pub(crate) fn apply_imported_snapshot(&mut self) {
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if let Ok(session) = zalo_client::StoredSession::load(&dir) {
            self.has_session = session.zpw_sek().is_some();
            if let Some(name) = session.display_name.filter(|item| !item.trim().is_empty()) {
                self.display_name = Some(name);
            }
            self.uin = session.uin;
            self.send2me_id = session.send2me_id;
        }
        self.profiles = crate::profiles::Profiles::load(Some(&dir.join("profiles.json")));
        self.prefs = crate::prefs::Prefs::load_scoped(
            crate::data::config_dir().filter(|path| path.is_dir()),
            Some(dir.clone()),
        );
        if let Some(snap) = crate::data::snapshot_from_store(&dir) {
            let keep = self
                .selected
                .filter(|id| snap.convs.iter().any(|conv| conv.id == *id));
            self.convs = snap.convs;
            self.messages = snap.messages;
            self.next_msg_id = snap.next_msg_id;
            self.selected = keep.or_else(|| self.convs.first().map(|conv| conv.id));
        }
        self.info_shelf = None;
        self.info_join.clear();
        if !cfg!(test) {
            self.mark_realtime_stale();
        }
    }

    /// Xuất / Nhập sheet, or the confirm step before a zip replaces the account.
    pub(crate) fn transfer_popup(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.export_ui.popup == TransferPopup::ImportAsk {
            return self.import_ask_dialog(cx);
        }
        self.transfer_dialog(cx)
    }

    fn import_ask_dialog(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let later = str_vi("STR_BACKUP_MSG_POPUP_BTN_LATER", "Để sau").to_owned();
        div()
            .id("pc-import-ask-layer")
            .absolute()
            .inset(px(0.))
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(crate::color::modal_scrim())
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.on_transfer_click(TransferClick::Later, cx);
            }))
            .child(
                div()
                    .id("pc-import-ask")
                    .occlude()
                    .w(px(460.))
                    .p(px(22.))
                    .rounded(px(8.))
                    .bg(rgb(paper))
                    .shadow_lg()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .text_size(px(16.))
                            .line_height(px(22.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child(str_vi("STR_IMPORT", "Nhập dữ liệu").to_owned()),
                    )
                    .child(
                        div()
                            .mt(px(12.))
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(rgb(ink))
                            .child("Dữ liệu tài khoản trên máy này sẽ được thay bằng file đã chọn."),
                    )
                    .child(
                        div()
                            .mt(px(8.))
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(rgb(ink))
                            .child("Tin nhắn đang dùng được giữ thành store.db.prev."),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.))
                            .mt(px(18.))
                            .child(self.sheet_button(
                                "pc-import-no",
                                later,
                                false,
                                true,
                                TransferClick::ImportNo,
                                cx,
                            ))
                            .child(self.sheet_button(
                                "pc-import-yes",
                                "Nhập dữ liệu".into(),
                                true,
                                true,
                                TransferClick::ImportYes,
                                cx,
                            )),
                    ),
            )
            .into_any_element()
    }

    fn transfer_dialog(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let exporting = self.export_ui.popup == TransferPopup::ExportForm;
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let field = token_rgb("--layer-background-subtle", "#f4f5f7");
        let line = crate::color::border_subtle();
        let file_name = self
            .export_ui
            .export_name
            .clone()
            .filter(|name| safe_export_name(name))
            .unwrap_or_else(|| "Chưa có tên file".into());
        let folder_text = self
            .export_ui
            .export_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "Chưa chọn thư mục".into());
        let chosen_file = self
            .export_ui
            .import_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Chưa chọn file".into());
        let installed = installed_zalo_dir();
        let places = zalo_places(zalo_install_dir(), installed.clone());
        let mut facts = div().flex().flex_col().gap(px(10.));
        if exporting {
            facts = facts.child(self.fact_text("Tên file", file_name, false));
        }
        if let Some(place) = places.iter().find(|place| !place.data) {
            let id = if exporting {
                "pc-export-install"
            } else {
                "pc-import-install"
            };
            facts = facts.child(self.fact_link(
                place.label,
                id,
                place.path.display().to_string(),
                TransferClick::OpenInstallDir,
                cx,
            ));
        }
        if let Some(place) = places.iter().find(|place| place.data) {
            let id = if exporting {
                "pc-export-data"
            } else {
                "pc-import-data"
            };
            facts = facts.child(self.fact_link(
                place.label,
                id,
                place.path.display().to_string(),
                TransferClick::OpenZaloDir,
                cx,
            ));
        }
        if !exporting && installed.is_none() {
            facts = facts.child(self.fact_text(
                "Đường dẫn",
                "Máy này chưa cài Zalo. Cần cài Zalo trước khi nhập dữ liệu.".into(),
                true,
            ));
        }
        let (place_text, place_btn, place_click) = if exporting {
            (folder_text, "Thay đổi", TransferClick::ExportChange)
        } else {
            (chosen_file, "Chọn File", TransferClick::ImportChoose)
        };
        let place_id = if exporting {
            "pc-place-change"
        } else {
            "pc-place-choose"
        };
        let place_label = if exporting {
            "Nơi lưu file"
        } else {
            "Nơi lưu bản sao lưu"
        };
        let can_import = installed.is_some() && self.export_ui.import_path.is_some();
        let mut actions = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap(px(8.))
            .mt(px(18.))
            .child(self.sheet_button(
                "pc-sheet-later",
                "Để sau".into(),
                false,
                true,
                TransferClick::Later,
                cx,
            ));
        if exporting {
            actions = actions.child(self.sheet_button(
                "pc-sheet-export",
                "Xuất dữ liệu".into(),
                true,
                true,
                TransferClick::ExportRun,
                cx,
            ));
        } else {
            actions = actions.child(self.sheet_button(
                "pc-sheet-import",
                "Nhập dữ liệu".into(),
                true,
                can_import,
                TransferClick::ImportRun,
                cx,
            ));
        }
        div()
            .id("pc-transfer-layer")
            .absolute()
            .inset(px(0.))
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(crate::color::modal_scrim())
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.on_transfer_click(TransferClick::Later, cx);
            }))
            .child(
                div()
                    .id("pc-transfer")
                    .occlude()
                    .w(px(560.))
                    .p(px(20.))
                    .rounded(px(8.))
                    .bg(rgb(paper))
                    .shadow_lg()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .text_size(px(16.))
                            .line_height(px(22.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child("Xuất dữ liệu / Nhập dữ liệu"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .mt(px(14.))
                            .border_b_1()
                            .border_color(line)
                            .child(self.transfer_tab(
                                "pc-tab-export",
                                "Xuất dữ liệu",
                                exporting,
                                TransferClick::ShowExport,
                                cx,
                            ))
                            .child(self.transfer_tab(
                                "pc-tab-import",
                                "Nhập dữ liệu",
                                !exporting,
                                TransferClick::ShowImport,
                                cx,
                            )),
                    )
                    .child(facts.mt(px(16.)))
                    .child(
                        div()
                            .mt(px(16.))
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child(place_label),
                    )
                    .child(
                        div()
                            .mt(px(8.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .h(px(32.))
                                    .px(px(12.))
                                    .rounded(px(6.))
                                    .bg(rgb(field))
                                    .border_1()
                                    .border_color(line)
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .text_size(px(13.))
                                            .text_color(rgb(ink))
                                            .whitespace_nowrap()
                                            .truncate()
                                            .child(place_text),
                                    ),
                            )
                            .child(self.sheet_button(
                                place_id,
                                place_btn.into(),
                                false,
                                true,
                                place_click,
                                cx,
                            )),
                    )
                    .child(actions),
            )
            .into_any_element()
    }

    fn fact_text(&self, label: &'static str, value: String, wrap: bool) -> gpui::Div {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .min_h(px(22.))
            .child(
                div()
                    .w(px(148.))
                    .flex_none()
                    .text_size(px(13.))
                    .text_color(rgb(muted))
                    .whitespace_nowrap()
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .text_color(rgb(ink))
                    .when(!wrap, |el| el.whitespace_nowrap().truncate())
                    .child(value),
            )
    }

    fn fact_link(
        &self,
        label: &'static str,
        id: &'static str,
        value: String,
        click: TransferClick,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(12.))
            .min_h(px(22.))
            .child(
                div()
                    .w(px(148.))
                    .flex_none()
                    .text_size(px(13.))
                    .line_height(px(20.))
                    .text_color(rgb(muted))
                    .whitespace_nowrap()
                    .child(label),
            )
            .child(
                div()
                    .id(id)
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .text_color(rgb(blue))
                    .cursor_pointer()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.on_transfer_click(click, cx);
                    }))
                    .child(soft_export_path(&value)),
            )
    }

    fn transfer_tab(
        &self,
        id: &'static str,
        label: &'static str,
        on: bool,
        click: TransferClick,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        div()
            .id(id)
            .flex_1()
            .h(px(36.))
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.on_transfer_click(click, cx);
            }))
            .child(
                div()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(if on { blue } else { muted }))
                    .whitespace_nowrap()
                    .child(label),
            )
            .child(
                div()
                    .mt(px(8.))
                    .h(px(2.))
                    .w_full()
                    .when(on, |el| el.bg(rgb(blue))),
            )
    }

    fn sheet_button(
        &self,
        id: &'static str,
        label: String,
        primary: bool,
        enabled: bool,
        click: TransferClick,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-disabled", "#a6afbd");
        let blue = token_rgb("--B55", "#0068ff");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let bg = if !enabled {
            subtle
        } else if primary {
            blue
        } else {
            subtle
        };
        let fg = if !enabled {
            muted
        } else if primary {
            crate::color::on_accent_fg()
        } else {
            ink
        };
        div()
            .id(id)
            .h(px(32.))
            .px(px(14.))
            .flex_none()
            .rounded(px(4.))
            .flex()
            .items_center()
            .bg(rgb(bg))
            .text_size(px(13.))
            .font_family(FAMILY_SEMIBOLD)
            .text_color(rgb(fg))
            .whitespace_nowrap()
            .when(enabled, |el| {
                el.cursor_pointer()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.on_transfer_click(click, cx);
                    }))
            })
            .child(label)
    }

    fn on_transfer_click(&mut self, click: TransferClick, cx: &mut Context<Self>) {
        match click {
            TransferClick::Later => self.export_ui.popup = TransferPopup::Closed,
            TransferClick::ImportNo => {
                self.export_ui.popup = TransferPopup::Closed;
                self.export_ui.import_path = None;
            }
            TransferClick::ImportYes => self.finish_pc_import(cx),
            TransferClick::ExportChange => self.change_export_folder(cx),
            TransferClick::ExportRun => self.commit_pc_export(cx),
            TransferClick::ImportChoose => self.choose_import_file(cx),
            TransferClick::ImportRun => {
                if self.export_ui.import_path.is_some() {
                    self.export_ui.popup = TransferPopup::ImportAsk;
                }
            }
            TransferClick::ShowExport => self.show_pc_export_tab(),
            TransferClick::ShowImport => self.show_pc_import_tab(),
            TransferClick::OpenInstallDir => self.open_install_folder(),
            TransferClick::OpenZaloDir => self.open_zalo_data_folder(),
        }
        cx.notify();
    }
}

fn soft_export_path(path: &str) -> String {
    path.replace('/', "/\u{200b}")
}

fn import_destination(zip_path: &Path, fallback: &Path) -> PathBuf {
    let Some(uid) = packaged_user_id(zip_path) else {
        return fallback.to_path_buf();
    };
    let Some(machine) = crate::data::config_dir() else {
        return fallback.to_path_buf();
    };
    crate::data::account_dir(&machine, &uid)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pc_export_roundtrip_stamp_and_version1() {
        assert_eq!(
            export_stamp_name(0, "NCT 89 PC", "Ubuntu"),
            "01011970-070000-nct-89-pc-zalo-ubuntu.zip"
        );
        assert!(safe_export_name("01011970-070000-desk-top-zalo-linux.zip"));
        assert!(!safe_export_name("../x.zip"));

        let root = std::env::temp_dir().join(format!("zalo-export-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let account = root.join("machine").join("accounts").join("user");
        std::fs::create_dir_all(&account).unwrap();
        let db = account.join("store.db");
        {
            let store = zalo_storage::LocalStore::open(&db).unwrap();
            drop(store);
        }
        std::fs::write(account.join("store.db-wal"), b"wal").unwrap();
        std::fs::write(account.join("backup-session.json"), b"{}").unwrap();
        std::fs::write(account.join("session.json"), br#"{"user_id":"42"}"#).unwrap();
        std::fs::write(
            account.join("device.json"),
            br#"{"computer_name":"Desk Top","imei":"abc"}"#,
        )
        .unwrap();
        std::fs::write(account.join("account.uid"), b"42").unwrap();
        std::fs::write(account.join("profiles.json"), b"{\"map\":{}}").unwrap();
        std::fs::write(root.join("machine").join("ui-state.json"), b"{\"pinned\":[]}").unwrap();
        std::fs::write(root.join("machine").join("gpui-prefs.json"), b"{\"labels\":{}}").unwrap();

        let stamped = export_file_name_for(&account);
        assert!(stamped.ends_with(".zip"));
        assert!(stamped.contains("-desk-top-zalo-"));
        assert!(safe_export_name(&stamped));

        let dest = root.join("out.zip");
        export_store(&account, &dest).unwrap();
        let packed = unzip_store(&std::fs::read(&dest).unwrap()).unwrap();
        let names: Vec<&str> = packed.iter().map(|(name, _)| name.as_str()).collect();
        for want in [
            "manifest.json",
            "store.db",
            "session.json",
            "device.json",
            "account.uid",
            "profiles.json",
            "prefs-ui-state.json",
            "prefs-gpui.json",
        ] {
            assert!(names.contains(&want), "{want}");
        }
        assert!(!names.iter().any(|name| name.contains("wal") || name.contains("backup")));
        let manifest = String::from_utf8_lossy(entry_bytes(&packed, "manifest.json").unwrap());
        assert!(manifest.contains("\"version\":2"));
        assert_eq!(packaged_user_id(&dest).as_deref(), Some("42"));

        let other = root.join("machine2").join("accounts").join("user");
        std::fs::create_dir_all(&other).unwrap();
        import_store(&other, &dest).unwrap();
        assert!(other.join("store.db").is_file());
        assert_eq!(std::fs::read(other.join("account.uid")).unwrap(), b"42");
        assert_eq!(
            std::fs::read(root.join("machine2").join("device.json")).unwrap(),
            std::fs::read(account.join("device.json")).unwrap()
        );
        assert_eq!(
            std::fs::read(root.join("machine2").join("ui-state.json")).unwrap(),
            b"{\"pinned\":[]}"
        );
        assert!(!other.join("store.db-wal").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(other.join("session.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let v1_dir = root.join("v1");
        std::fs::create_dir_all(&v1_dir).unwrap();
        let db_bytes = std::fs::read(&db).unwrap();
        let v1 = zip_store(&[
            (MANIFEST, br#"{"kind":"zalo-rust-pc-export","version":1}"#),
            (DB_NAME, &db_bytes),
        ]);
        let v1_path = root.join("v1.zip");
        std::fs::write(&v1_path, v1).unwrap();
        import_store(&v1_dir, &v1_path).unwrap();
        assert!(v1_dir.join("store.db").is_file());
        assert!(!v1_dir.join("session.json").is_file());

        let foreign_path = root.join("foreign.zip");
        std::fs::write(&foreign_path, zip_store(&[("readme.txt", b"nope")])).unwrap();
        let err = import_store(&v1_dir, &foreign_path).unwrap_err();
        assert!(err.contains("không phải gói xuất"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn export_places_are_install_and_data_not_the_zip() {
        let install = PathBuf::from("/tmp/zalo-install-a");
        let data = PathBuf::from("/tmp/zalo-data-b");
        let places = zalo_places(Some(install.clone()), Some(data.clone()));
        assert_eq!(places.len(), 2);
        assert_eq!(places[0].label, "Nơi cài");
        assert_eq!(places[0].path, install);
        assert!(!places[0].data);
        assert_eq!(places[1].label, "Nơi chứa dữ liệu");
        assert_eq!(places[1].path, data);
        assert!(places[1].data);
        let one = zalo_places(Some(install.clone()), Some(install.clone()));
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].label, "Nơi cài");
        assert!(zalo_places(None, None).is_empty());
        let data_only = zalo_places(None, Some(data));
        assert_eq!(data_only.len(), 1);
        assert!(data_only[0].data);
    }
}
