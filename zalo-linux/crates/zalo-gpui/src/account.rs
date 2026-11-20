//! Account profile, group management, and `.zdb4` backup decryption dialogs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    div, img, prelude::*, px, rgb, ClickEvent, ClipboardItem, Context, Entity, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, RenderImage, StatefulInteractiveElement, Styled,
};
use serde_json::Value;
use zalo_client::GroupSettingsPatch;

use crate::color::{str_vi, token_rgb};
use crate::dialogs::{dialog_footer, Dialog, Person};
use crate::fonts::{self, ic, FAMILY_SEMIBOLD};
use crate::line_input::{LineEvent, LineInput};
use crate::shell::ShellView;

/// Role label for the signed-in account inside a group.
#[must_use]
pub(crate) fn group_role_label(
    mine: &HashSet<String>,
    creator: &str,
    admins: &[String],
) -> &'static str {
    if !creator.is_empty() && mine.contains(creator) {
        "Trưởng nhóm"
    } else if admins.iter().any(|id| mine.contains(id)) {
        "Phó nhóm"
    } else {
        "Thành viên"
    }
}

/// `bizInfo.biz` fields Zalo PC shows under "Business Information".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BizCard {
    /// `bizInfo.pkgId` (or `bizPkg.pkgId`). Non-zero means a zBusiness account.
    pub pkg_id: i64,
    /// Business description (`biz.desc`).
    pub desc: String,
    /// Business address.
    pub addr: String,
    /// Business website.
    pub website: String,
    /// Business email.
    pub email: String,
}

/// Extracted summary of `/api/social/profile/me`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AccountMe {
    /// Numeric Zalo user id.
    pub uid: String,
    /// Public display name.
    pub display_name: String,
    /// Underlying Zalo handle.
    pub zalo_name: String,
    /// `0` = Male, `1` = Female.
    pub gender: i64,
    /// Birthday formatted as `dd/mm/yyyy` for editing.
    pub birthday: String,
    /// Registered phone number if returned.
    pub phone: String,
    /// Profile bio/status line if returned.
    pub status: String,
    /// Cover photo URL when the server returns one.
    pub cover: String,
    /// zBusiness card. Empty when the account is not a business profile.
    pub biz: BizCard,
}

fn json_text(root: &serde_json::Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| match root.get(*key)? {
            serde_json::Value::String(s) => {
                let t = s.trim();
                (!t.is_empty()).then(|| t.to_owned())
            }
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Formats `YYYY-MM-DD` or `DD/MM/YYYY` or unix timestamp seconds into `dd/mm/yyyy`.
fn normalize_display_birthday(root: &serde_json::Value) -> String {
    let raw = json_text(root, &["sdob"]);
    if !raw.is_empty() {
        let parts: Vec<&str> = raw.split(['/', '-']).collect();
        if let [a, b, c] = parts.as_slice() {
            if a.len() == 4 {
                return format!("{c:0>2}/{b:0>2}/{a}");
            }
            if c.len() == 4 {
                return format!("{a:0>2}/{b:0>2}/{c}");
            }
        }
        return raw;
    }
    if let Some(secs) = root
        .get("dob")
        .and_then(serde_json::Value::as_i64)
        .filter(|s| *s != 0)
    {
        let (y, m, d) = crate::data::vn_ymd(secs * 1000);
        return format!("{d:02}/{m:02}/{y}");
    }
    String::new()
}

/// Parses `/api/social/profile/me` or `/api/social/profile/me-v2` into [`AccountMe`].
#[must_use]
pub(crate) fn me_from_value(value: &serde_json::Value) -> AccountMe {
    let root = flatten_me(value);
    let gender = root
        .get("gender")
        .and_then(json_i64)
        .filter(|&g| g == 0 || g == 1)
        .unwrap_or(0);
    AccountMe {
        uid: json_text(&root, &["userId", "uid", "id"]),
        display_name: json_text(
            &root,
            &["displayName", "display_name", "zaloName", "zalo_name"],
        ),
        zalo_name: json_text(&root, &["zaloName", "zalo_name"]),
        gender,
        birthday: normalize_display_birthday(&root),
        phone: json_text(&root, &["phoneNumber"]),
        status: json_text(&root, &["status"]),
        cover: cover_url(&root),
        biz: biz_from(&root),
    }
}

fn json_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_u64().map(|u| u as i64)),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Object, or a JSON string that decodes to an object.
fn object_value(value: &serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::Object(_) => Some(value.clone()),
        serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
            .ok()
            .filter(|v| v.is_object()),
        _ => None,
    }
}

/// Electron `getProfileMe` copies `profile`, replaces `bizInfo` with `bizPkg`,
/// then stores the sibling `biz` object on `bizInfo.biz`.
fn flatten_me(value: &serde_json::Value) -> serde_json::Value {
    let outer = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let Some(mut profile) = outer.get("profile").and_then(object_value) else {
        return outer.clone();
    };
    if let Some(pkg) = profile.get("bizPkg").and_then(object_value) {
        if let Some(obj) = profile.as_object_mut() {
            obj.insert("bizInfo".to_owned(), pkg);
            obj.remove("bizPkg");
        }
    }
    if let Some(biz) = outer.get("biz").and_then(object_value) {
        if let Some(obj) = profile.as_object_mut() {
            let slot = obj
                .entry("bizInfo".to_owned())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            if !slot.is_object() {
                *slot = serde_json::Value::Object(serde_json::Map::new());
            }
            if let Some(info) = slot.as_object_mut() {
                info.insert("biz".to_owned(), biz);
            }
        }
    }
    profile
}

fn cover_url(root: &serde_json::Value) -> String {
    if let Some(obj) = root.get("cover").filter(|v| v.is_object()) {
        let nested = json_text(obj, &["url", "src", "coverUrl"]);
        if nested.starts_with("http") {
            return nested;
        }
    }
    json_text(root, &["cover", "coverUrl"])
}

fn biz_from(root: &serde_json::Value) -> BizCard {
    let info = root
        .get("bizInfo")
        .and_then(object_value)
        .or_else(|| root.get("bizPkg").and_then(object_value));
    let pkg_id = info
        .as_ref()
        .and_then(|b| b.get("pkgId"))
        .and_then(json_i64)
        .or_else(|| {
            root.get("bizPkg")
                .and_then(|b| b.get("pkgId"))
                .and_then(json_i64)
        })
        .unwrap_or(0);
    let biz = info
        .as_ref()
        .and_then(|b| b.get("biz"))
        .and_then(object_value);
    let pick = |keys: &[&str]| biz.as_ref().map(|b| json_text(b, keys)).unwrap_or_default();
    BizCard {
        pkg_id,
        desc: pick(&["desc", "description"]),
        addr: pick(&["addr", "address"]),
        website: pick(&["website", "url"]),
        email: pick(&["email"]),
    }
}

/// Zalo's placeholder cover ends with `default` and is not a photo.
fn cover_is_photo(url: &str) -> bool {
    let url = url.trim();
    url.starts_with("https://") && !url.ends_with("default")
}

/// Formats `21/04/1988` into a localized long date string (Zalo PC profile row).
#[must_use]
pub(crate) fn pretty_birthday(raw: &str) -> String {
    let parts: Vec<&str> = raw.split(['/', '-']).collect();
    match parts.as_slice() {
        [d, m, y] if y.len() == 4 && d.len() <= 2 && m.len() <= 2 => {
            format!("{d:0>2} tháng {m:0>2}, {y}")
        }
        [y, m, d] if y.len() == 4 && d.len() <= 2 && m.len() <= 2 => {
            format!("{d:0>2} tháng {m:0>2}, {y}")
        }
        _ => raw.to_owned(),
    }
}

/// `84886486872` or `+84898486872` → `+84 886 486 872` when the local part is 9 digits.
#[must_use]
pub(crate) fn pretty_phone(raw: &str) -> String {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let Some(rest) = digits.strip_prefix("84").filter(|r| !r.is_empty()) else {
        return if raw.trim().is_empty() {
            "—".to_owned()
        } else {
            raw.trim().to_owned()
        };
    };
    if rest.len() == 9 {
        format!("+84 {} {} {}", &rest[0..3], &rest[3..6], &rest[6..9])
    } else {
        format!("+84 {rest}")
    }
}

fn gender_word(gender: i64) -> &'static str {
    if gender == 1 {
        str_vi("STR_GENDER_FEMALE", "Nữ")
    } else {
        str_vi("STR_GENDER_MALE", "Nam")
    }
}

/// Converts `dd/mm/yyyy` (or already-ISO `yyyy-mm-dd`) into `YYYY-MM-DD` for `/api/social/profile/update`.
#[must_use]
pub(crate) fn iso_birthday(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let (d_str, m_str, y_str) = if let [d, m, y] = trimmed.split('/').collect::<Vec<_>>().as_slice()
    {
        (*d, *m, *y)
    } else if let [y, m, d] = trimmed.split('-').collect::<Vec<_>>().as_slice() {
        (*d, *m, *y)
    } else {
        return None;
    };
    if y_str.len() != 4 {
        return None;
    }
    let d: u32 = d_str.parse().ok()?;
    let m: u32 = m_str.parse().ok()?;
    let y: u32 = y_str.parse().ok()?;
    if !(1..=31).contains(&d) || !(1..=12).contains(&m) || !(1900..=2100).contains(&y) {
        return None;
    }
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Validates and normalizes display name, gender (`0 | 1`), and birthday before `update_profile`.
pub(crate) fn validate_profile_input(
    name: &str,
    gender: i64,
    birthday: &str,
) -> Result<(String, i64, String), &'static str> {
    let clean = name.trim();
    if clean.is_empty() {
        return Err("Tên hiển thị không được để trống.");
    }
    let iso = iso_birthday(birthday).ok_or("Ngày sinh phải theo dạng dd/mm/yyyy.")?;
    let clean_gender = if gender == 1 { 1 } else { 0 };
    Ok((clean.to_owned(), clean_gender, iso))
}

/// Strips directory segments and rejects traversal entries from ZDB4 table names.
#[must_use]
pub(crate) fn safe_zdb4_entry_name(raw: &str) -> Option<String> {
    let leaf = raw.rsplit(['/', '\\']).next()?.trim();
    if leaf.is_empty() || leaf == "." || leaf == ".." {
        return None;
    }
    Some(leaf.to_owned())
}

/// Default output folder under `$XDG_DATA_HOME/zalo-rust/zdb4-extracted`.
#[must_use]
pub(crate) fn default_zdb4_out_dir() -> PathBuf {
    zalo_core::XdgDirs::from_env()
        .map(|xdg| xdg.data_home().join("zalo-rust").join("zdb4-extracted"))
        .unwrap_or_else(|_| std::env::temp_dir().join("zalo-rust-zdb4-extracted"))
}

/// Reads and decrypts a `.zdb4` backup archive, writing extracted files into `out_dir`.
///
/// Returns `(file_count, summary_lines)`.
pub(crate) fn decrypt_zdb4_to_dir(
    path: &Path,
    key: &str,
    out_dir: &Path,
) -> Result<(usize, Vec<String>), String> {
    let path_trimmed = path.as_os_str();
    let key = key.trim();
    if path_trimmed.is_empty() || key.is_empty() {
        return Err("Cần nhập đường dẫn file .zdb4 và khóa giải mã.".into());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Không đọc được tệp: {e}"))?;
    let files = zalo_storage::decrypt_zdb4_v2(&bytes, key)
        .map_err(|e| format!("Giải mã ZDB4 thất bại: {e}"))?;
    std::fs::create_dir_all(out_dir).map_err(|e| format!("Không tạo được thư mục đích: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(out_dir, std::fs::Permissions::from_mode(0o700));
    }
    let mut written = Vec::new();
    for file in &files {
        let Some(safe_name) = safe_zdb4_entry_name(&file.name) else {
            continue;
        };
        let target = out_dir.join(&safe_name);
        std::fs::write(&target, &file.bytes)
            .map_err(|e| format!("Không ghi được tệp {safe_name}: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
        }
        written.push(format!("{safe_name} ({} byte)", file.bytes.len()));
    }
    Ok((files.len(), written))
}

/// Active tab inside "Group Management".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupManageTab {
    #[default]
    Permissions,
    Pending,
    Link,
}

/// Identifier of each group setting switch in `/api/group/setting/update`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupSettingKey {
    BlockName,
    SignAdminMsg,
    LockCreatePost,
    LockCreatePoll,
    JoinAppr,
    LockSendMsg,
    LockViewMember,
}

/// Catalogue keys for the seven `/api/group/setting/update` flags.
/// `blockName` is one field: the rename sentence is the label and the avatar
/// sentence is the subtitle. There is no separate pin-message flag in the payload.
const GROUP_SETTING_ROWS: &[(GroupSettingKey, &str, &str)] = &[
    (
        GroupSettingKey::BlockName,
        "STR_TOAST_ADMIN_RENAME",
        "STR_ADMIN_CHANGE_AVATAR",
    ),
    (GroupSettingKey::SignAdminMsg, "STR_MARK_ADMIN_MESSAGE", ""),
    (
        GroupSettingKey::LockCreatePost,
        "STE_ADMIN_CREATE_TOPIC",
        "",
    ),
    (GroupSettingKey::LockCreatePoll, "STE_ADMIN_CREATE_POLL", ""),
    (
        GroupSettingKey::JoinAppr,
        "STR_ADMIN_MEMBERSHIP_APPROVAL",
        "",
    ),
    (
        GroupSettingKey::LockSendMsg,
        "STR_LOCK_SEND_MESSAGE_MESSAGE_FULL",
        "",
    ),
    (
        GroupSettingKey::LockViewMember,
        "STR_GROUP_PRIVACY_HIDE_MEMBER_NOTE",
        "",
    ),
];

fn setting_copy(key: &'static str, fallback: &'static str) -> String {
    crate::color::str_vi(key, fallback).trim().to_owned()
}

/// Parsed group permission flags from `gridInfoMap.<grid>.setting`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupSettingsState {
    pub block_name: bool,
    pub sign_admin_msg: bool,
    pub lock_create_post: bool,
    pub lock_create_poll: bool,
    pub join_appr: bool,
    pub lock_send_msg: bool,
    pub lock_view_member: bool,
}

impl GroupSettingsState {
    fn get(&self, key: GroupSettingKey) -> bool {
        match key {
            GroupSettingKey::BlockName => self.block_name,
            GroupSettingKey::SignAdminMsg => self.sign_admin_msg,
            GroupSettingKey::LockCreatePost => self.lock_create_post,
            GroupSettingKey::LockCreatePoll => self.lock_create_poll,
            GroupSettingKey::JoinAppr => self.join_appr,
            GroupSettingKey::LockSendMsg => self.lock_send_msg,
            GroupSettingKey::LockViewMember => self.lock_view_member,
        }
    }

    /// Applies one flag change locally and returns the matching API patch.
    #[must_use]
    pub fn with_change(&self, key: GroupSettingKey, next: bool) -> (Self, GroupSettingsPatch) {
        let mut updated = self.clone();
        let bit = Some(i64::from(next));
        let mut patch = GroupSettingsPatch::default();
        match key {
            GroupSettingKey::BlockName => {
                updated.block_name = next;
                patch.block_name = bit;
            }
            GroupSettingKey::SignAdminMsg => {
                updated.sign_admin_msg = next;
                patch.sign_admin_msg = bit;
            }
            GroupSettingKey::LockCreatePost => {
                updated.lock_create_post = next;
                patch.lock_create_post = bit;
            }
            GroupSettingKey::LockCreatePoll => {
                updated.lock_create_poll = next;
                patch.lock_create_poll = bit;
            }
            GroupSettingKey::JoinAppr => {
                updated.join_appr = next;
                patch.join_appr = bit;
            }
            GroupSettingKey::LockSendMsg => {
                updated.lock_send_msg = next;
                patch.lock_send_msg = bit;
            }
            GroupSettingKey::LockViewMember => {
                updated.lock_view_member = next;
                patch.lock_view_member = bit;
            }
        }
        (updated, patch)
    }
}

/// Pending join request awaiting admin approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingMember {
    pub uid: String,
    pub name: String,
}

/// Group invite link metadata from `/api/group/link/detail`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupLinkState {
    pub url: String,
    pub enabled: bool,
}

fn json_flag(v: &Value, key: &str) -> bool {
    v.get(key)
        .and_then(|val| val.as_i64().map(|n| n != 0).or_else(|| val.as_bool()))
        .unwrap_or(false)
}

/// Extracts [`GroupSettingsState`] from a `get_group_info_v2` JSON payload.
#[must_use]
pub fn group_settings_from(value: &Value) -> GroupSettingsState {
    let root = value.get("data").unwrap_or(value);
    let group_obj = root
        .get("gridInfoMap")
        .and_then(Value::as_object)
        .and_then(|m| m.values().next())
        .unwrap_or(root);
    let s = group_obj.get("setting").unwrap_or(group_obj);
    GroupSettingsState {
        block_name: json_flag(s, "blockName"),
        sign_admin_msg: json_flag(s, "signAdminMsg"),
        lock_create_post: json_flag(s, "lockCreatePost"),
        lock_create_poll: json_flag(s, "lockCreatePoll"),
        join_appr: json_flag(s, "joinAppr"),
        lock_send_msg: json_flag(s, "lockSendMsg"),
        lock_view_member: json_flag(s, "lockViewMember"),
    }
}

/// Electron module `6E7Z`: `COMMUNITY = 2`.
pub(crate) const GROUP_TYPE_COMMUNITY: i64 = 2;
/// Electron default `group_privacy.community.grouptype_threshold`.
pub(crate) const COMMUNITY_MEMBER_THRESHOLD: i64 = 100;

/// `type` and `totalMember` from a `getgi/v2` reply.
#[must_use]
pub(crate) fn group_kind_from(value: &Value) -> (Option<i64>, Option<i64>) {
    let root = value.get("data").unwrap_or(value);
    let group_obj = root
        .get("gridInfoMap")
        .and_then(Value::as_object)
        .and_then(|m| m.values().next())
        .unwrap_or(root);
    (
        field_i64(group_obj, "type"),
        field_i64(group_obj, "totalMember"),
    )
}

/// Electron `isCommunityFromInfo`: type 2, or more members than the default threshold.
#[must_use]
pub(crate) fn is_community(group_type: Option<i64>, total_member: Option<i64>) -> bool {
    group_type == Some(GROUP_TYPE_COMMUNITY)
        || total_member.is_some_and(|n| n > COMMUNITY_MEMBER_THRESHOLD)
}

fn ttl_caption(ms: i64) -> String {
    const DAY: i64 = 86_400_000;
    if ms > 0 && ms % DAY == 0 {
        let days = ms / DAY;
        return format!("{days} ngày");
    }
    format!("{ms} ms")
}

fn field_i64(value: &Value, key: &str) -> Option<i64> {
    let v = value.get(key)?;
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Parses pending group join requests from `/api/group/pending-mems/list`.
#[must_use]
pub fn group_pending_from(value: &Value) -> Vec<PendingMember> {
    let root = value.get("data").unwrap_or(value);
    let Some(arr) = root
        .get("users")
        .and_then(Value::as_array)
        .or_else(|| root.as_array())
    else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let uid = ["uid", "userId", "id"]
                .iter()
                .find_map(|k| item.get(*k))
                .and_then(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .or_else(|| v.as_i64().map(|n| n.to_string()))
                })
                .filter(|s| !s.is_empty())?;
            let name = ["dpn", "displayName", "zaloName"]
                .iter()
                .find_map(|k| item.get(*k).and_then(Value::as_str))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&uid)
                .to_owned();
            Some(PendingMember { uid, name })
        })
        .collect()
}

/// Parses `/api/group/link/detail` or `/api/group/link/new` response into [`GroupLinkState`].
#[must_use]
pub fn group_link_from(value: &Value) -> GroupLinkState {
    let root = value.get("data").unwrap_or(value);
    let url = root
        .get("link")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    let enabled = root
        .get("enabled")
        .and_then(|v| v.as_i64().map(|n| n != 0).or_else(|| v.as_bool()))
        .unwrap_or(!url.is_empty());
    GroupLinkState { url, enabled }
}

/// "Group Management" modal state.
pub struct GroupManageDlg {
    pub(crate) conv_id: u64,
    pub(crate) thread_id: String,
    pub(crate) name: String,
    pub(crate) loading: bool,
    pub(crate) creator: String,
    pub(crate) admins: Vec<String>,
    pub(crate) member_count: usize,
    pub(crate) leave_armed: bool,
    /// Second click sends `silent: 1` when this is set.
    pub(crate) leave_silent: bool,
    /// Second click calls `/api/group/disperse`. Owner only. Never fired from tests.
    pub(crate) disperse_armed: bool,
    /// `type` from `getgi/v2`. `None` until the server answers.
    pub(crate) group_type: Option<i64>,
    /// `totalMember` from `getgi/v2`.
    pub(crate) total_member: Option<i64>,
    /// Second click calls `/api/group/upgrade/community`. Owner only. Never fired from tests.
    pub(crate) community_armed: bool,
    /// TTL from `getConvers` when this thread is in the server list.
    pub(crate) ttl_ms: Option<i64>,
    pub(crate) tab: GroupManageTab,
    pub(crate) settings: GroupSettingsState,
    pub(crate) settings_busy: bool,
    pub(crate) pending_loading: bool,
    pub(crate) pending: Vec<PendingMember>,
    pub(crate) link_loading: bool,
    pub(crate) link: GroupLinkState,
    /// Failure of the pending or link tab. Kept off the permissions banner.
    pub(crate) tab_note: Option<String>,
    pub(crate) error: Option<String>,
}

/// "Your Profile" account info + editor modal state.
pub struct ProfileDlg {
    pub(crate) me: AccountMe,
    pub(crate) name: Entity<LineInput>,
    pub(crate) dob: Entity<LineInput>,
    pub(crate) gender: i64,
    pub(crate) loading: bool,
    pub(crate) saving: bool,
    /// Read layout until "Update" / the name pencil opens the editor.
    pub(crate) editing: bool,
    pub(crate) error: Option<String>,
    pub(crate) status: Option<String>,
    /// Decoded cover photo. Empty while the default banner is showing.
    pub(crate) cover_img: Option<Arc<RenderImage>>,
    pub(crate) cover_busy: bool,
}

/// "Decrypt .zdb4 Backup" modal state.
pub struct BackupDecryptDlg {
    pub(crate) path: Entity<LineInput>,
    pub(crate) key: Entity<LineInput>,
    pub(crate) out_dir: PathBuf,
    pub(crate) busy: bool,
    pub(crate) result: Option<Result<String, String>>,
}

impl ShellView {
    /// Opens "Group Management" on the permissions tab.
    pub(crate) fn open_group_manage(&mut self, conv_id: u64, cx: &mut Context<Self>) {
        self.open_group_manage_tab(conv_id, GroupManageTab::Permissions, cx);
    }

    /// Opens "Group Management" for group conversation `conv_id`, loading creator/admin roles.
    pub(crate) fn open_group_manage_tab(
        &mut self,
        conv_id: u64,
        tab: GroupManageTab,
        cx: &mut Context<Self>,
    ) {
        let Some(conv) = self.conv(conv_id).cloned().filter(|c| c.is_group) else {
            return;
        };
        let cached_count = self
            .members_known(&conv.thread_id)
            .map(|n| n as usize)
            .unwrap_or_else(|| self.members_of(&conv).len().max(1));
        let can_fetch =
            !cfg!(test) && self.has_session && crate::data::live_allowed(&conv.thread_id);
        let tid = conv.thread_id.clone();
        self.info_open = true;
        self.dialog = Some(Dialog::GroupManage(GroupManageDlg {
            conv_id,
            thread_id: conv.thread_id,
            name: conv.name,
            loading: can_fetch,
            creator: String::new(),
            admins: Vec::new(),
            member_count: cached_count,
            leave_armed: false,
            leave_silent: false,
            disperse_armed: false,
            group_type: None,
            total_member: None,
            community_armed: false,
            ttl_ms: None,
            tab,
            settings: GroupSettingsState::default(),
            settings_busy: false,
            pending_loading: false,
            pending: Vec::new(),
            link_loading: false,
            link: GroupLinkState::default(),
            tab_note: None,
            error: (!can_fetch).then(|| "Chế độ xem trước — chưa nối máy chủ".to_owned()),
        }));
        if !can_fetch {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let info = api
                            .get_group_info_v2(&tid, 1)
                            .await
                            .map_err(|e| e.to_string())?;
                        let ttl = api.get_auto_delete_conversations().await.ok();
                        Ok((info, ttl))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let mine = view.self_member_ids();
                let mut admin = false;
                if let Some(Dialog::GroupManage(d)) = view.dialog.as_mut() {
                    if d.conv_id == conv_id {
                        d.loading = false;
                        match res {
                            Ok((val, ttl)) => {
                                let (list, creator, admins) = crate::info::members_from(&val);
                                d.creator = creator;
                                d.admins = admins;
                                d.settings = group_settings_from(&val);
                                let (kind, total) = group_kind_from(&val);
                                d.group_type = kind;
                                d.total_member = total;
                                if let Some(value) = ttl.as_ref() {
                                    d.ttl_ms = zalo_client::features::autodelete_ttl_ms(
                                        value,
                                        &d.thread_id,
                                    );
                                }
                                if !list.is_empty() {
                                    d.member_count = list.len();
                                }
                                // Electron `getPendingMember` runs only for owner/admin.
                                // A regular member gets Zalo 114 "Invalid parameters".
                                admin =
                                    group_role_label(&mine, &d.creator, &d.admins) != "Thành viên";
                            }
                            Err(err) => d.error = Some(err),
                        }
                    }
                }
                if admin {
                    view.load_group_pending(conv_id, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn group_manage_el(
        &self,
        d: &GroupManageDlg,
        corners: crate::window_frame::CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let danger = token_rgb("--R60", "#c31818");
        let blue = token_rgb("--B55", "#0068ff");
        let mine = self.self_member_ids();
        let role = if d.loading {
            "Đang kiểm tra…".to_owned()
        } else {
            group_role_label(&mine, &d.creator, &d.admins).to_owned()
        };
        let grid = d
            .thread_id
            .strip_prefix("g_")
            .unwrap_or(&d.thread_id)
            .to_owned();
        let add_grid = grid.clone();
        let add_name = d.name.clone();
        let conv_id = d.conv_id;
        let leave_label = if d.leave_armed {
            "Xác nhận rời nhóm"
        } else {
            "Rời nhóm"
        };
        let owner = !d.loading && role == "Trưởng nhóm";
        let community = is_community(d.group_type, d.total_member);
        let disperse_label = if d.disperse_armed {
            "Xác nhận giải tán nhóm"
        } else {
            "Giải tán nhóm"
        };
        let community_label = if d.community_armed {
            "Xác nhận nâng cấp cộng đồng"
        } else {
            "Nâng cấp thành cộng đồng"
        };

        let action_btn = |id: &'static str, label: &'static str, is_danger: bool| {
            div()
                .id(id)
                .h(px(38.))
                .px(px(12.))
                .rounded(px(4.))
                .bg(rgb(subtle))
                .hover(move |st| st.bg(rgb(hover)))
                .flex()
                .items_center()
                .justify_between()
                .cursor_pointer()
                .text_size(px(14.))
                .text_color(rgb(if is_danger { danger } else { ink }))
                .child(label)
                .child(fonts::icon_el('\u{EA95}', "›", 16.))
        };

        corners
            .apply(
                div()
                    .id("conversation-info")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w(px(crate::info::INFO_PANEL_W))
                    .h_full()
                    .bg(rgb(paper))
                    .border_l_1()
                    .border_color(crate::color::border_subtle())
                    .text_color(rgb(ink)),
                false,
                false,
                false,
                true,
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .h(px(zalo_design::CHAT_HEADER_H))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(crate::color::border_subtle())
                    .child(
                        div()
                            .id("gm-back")
                            .size(px(32.))
                            .rounded(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |st| st.bg(rgb(hover)))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.dialog = None;
                                cx.notify();
                            }))
                            .child(fonts::icon_el('\u{EA8E}', "‹", 20.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_center()
                            .pr(px(32.))
                            .text_size(px(16.))
                            .font_family(FAMILY_SEMIBOLD)
                            .child("Quản lý nhóm"),
                    ),
            )
            .child(
                div()
                    .id("gm-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .px(px(16.))
                    .pt(px(14.))
                    .pb(px(32.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .p(px(12.))
                            .rounded(px(6.))
                            .bg(rgb(subtle))
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .truncate()
                                    .child(d.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(muted))
                                    .child(format!(
                                        "{} · Vai trò: {role}",
                                        crate::i18n::members(d.member_count)
                                    )),
                            )
                            .when(community, |card| {
                                card.child(
                                    div()
                                        .text_size(px(13.))
                                        .text_color(rgb(blue))
                                        .child("Cộng đồng"),
                                )
                            })
                            .when_some(d.ttl_ms, |card, ms| {
                                let text = if ms <= 0 {
                                    "Máy chủ báo tin tự xóa đang tắt. Máy này không gửi lệnh đổi.".to_owned()
                                } else {
                                    format!(
                                        "Tin tự xóa sau {}. Máy này không gửi lệnh đổi thời hạn.",
                                        ttl_caption(ms)
                                    )
                                };
                                card.child(
                                    div()
                                        .text_size(px(12.5))
                                        .text_color(rgb(muted))
                                        .child(text),
                                )
                            }),
                    )
                    .when_some(d.error.clone(), |el, err| {
                        el.child(div().text_size(px(12.5)).text_color(rgb(danger)).child(err))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(action_btn("gm-members", "Thành viên & xóa thành viên", false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.dialog = None;
                                    this.info_open = true;
                                    this.open_members(conv_id, cx);
                                    cx.notify();
                                }),
                            ))
                            .child(action_btn("gm-add", "Thêm thành viên", false).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.dialog = None;
                                    this.open_group_picker(Some((add_grid.clone(), add_name.clone())), cx);
                                    cx.notify();
                                },
                            )))
                            .child(action_btn("gm-rename", "Đổi tên nhóm", false).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.dialog = None;
                                    this.open_rename(conv_id, cx);
                                    cx.notify();
                                },
                            )))
                            .child(action_btn("gm-leave", leave_label, true).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let armed = matches!(
                                        this.dialog.as_ref(),
                                        Some(Dialog::GroupManage(dlg)) if dlg.leave_armed
                                    );
                                    let silent = matches!(
                                        this.dialog.as_ref(),
                                        Some(Dialog::GroupManage(dlg)) if dlg.leave_silent
                                    );
                                    if !armed {
                                        if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut() {
                                            dlg.leave_armed = true;
                                            dlg.leave_silent = false;
                                            dlg.disperse_armed = false;
                                            dlg.community_armed = false;
                                        }
                                    } else {
                                        this.dialog = None;
                                        this.leave_group_now(conv_id, silent, cx);
                                    }
                                    cx.notify();
                                },
                            )))
                            .when(owner, |col| {
                                let caption = setting_copy(
                                    "STR_CONFIRM_DELETE_GROUP",
                                    "Mời tất cả mọi người rời nhóm và xóa tin nhắn? Nhóm đã giải tán sẽ KHÔNG THỂ khôi phục.",
                                );
                                col.child(
                                    action_btn("gm-disperse", disperse_label, true).on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            let armed = matches!(
                                                this.dialog.as_ref(),
                                                Some(Dialog::GroupManage(dlg)) if dlg.disperse_armed
                                            );
                                            if !armed {
                                                if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut() {
                                                    dlg.disperse_armed = true;
                                                    dlg.leave_armed = false;
                                                    dlg.leave_silent = false;
                                                    dlg.community_armed = false;
                                                }
                                            } else {
                                                this.dialog = None;
                                                this.disperse_group_now(conv_id, cx);
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                                .when(d.disperse_armed, |col| {
                                    col.child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(rgb(danger))
                                            .child(caption),
                                    )
                                })
                                .when(!community, |col| {
                                    let confirm = "Nâng cấp không hoàn tác từ máy này. Bấm lần nữa để gửi /api/group/upgrade/community.";
                                    col.child(
                                        action_btn("gm-community", community_label, false).on_click(cx.listener(
                                            move |this, _, _, cx| {
                                                let armed = matches!(
                                                    this.dialog.as_ref(),
                                                    Some(Dialog::GroupManage(dlg)) if dlg.community_armed
                                                );
                                                if !armed {
                                                    if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut() {
                                                        dlg.community_armed = true;
                                                        dlg.disperse_armed = false;
                                                        dlg.leave_armed = false;
                                                        dlg.leave_silent = false;
                                                    }
                                                } else {
                                                    this.dialog = None;
                                                    this.upgrade_community_now(conv_id, cx);
                                                }
                                                cx.notify();
                                            },
                                        )),
                                    )
                                    .when(d.community_armed, |col| {
                                        col.child(
                                            div()
                                                .text_size(px(12.5))
                                                .text_color(rgb(muted))
                                                .child(confirm),
                                        )
                                    })
                                })
                            })
                            .when(d.leave_armed, |col| {
                                let caption = setting_copy(
                                    "STR_LEAVE_GROUP_SILENTLY_SWITCH_DESC",
                                    "Chỉ trưởng/phó nhóm biết bạn rời nhóm.",
                                );
                                col.child(
                                    div()
                                        .id("gm-leave-silent")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(8.))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut() {
                                                dlg.leave_silent = !dlg.leave_silent;
                                            }
                                            cx.notify();
                                        }))
                                        .child(crate::info::check_box(d.leave_silent))
                                        .child(div().flex_1().text_size(px(12.5)).text_color(rgb(muted)).child(caption)),
                                )
                            }),
                    )
                    .child(self.group_manage_advanced(d, cx)),
            )
    }

    /// Sends `/api/group/disperse` for the owner and hides the group row on success.
    ///
    /// The manage dialog already required a second click. This does not run
    /// while `cfg(test)` is set, and it does not run without a session.
    pub(crate) fn disperse_group_now(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(conv) = self.conv(id).cloned().filter(|c| c.is_group) else {
            return;
        };
        let grid = conv.thread_id;
        let Some(dir) = self
            .session_dir
            .clone()
            .filter(|_| self.has_session && !cfg!(test))
        else {
            self.set_note("Chưa đăng nhập — không giải tán được");
            return;
        };
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.disperse_group(&grid)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match r {
                    Ok(()) => {
                        view.removed.insert(id);
                        view.save_conv_flags();
                        view.selected = None;
                        view.info_open = false;
                        view.dialog = None;
                        view.set_note("Đã giải tán nhóm.");
                    }
                    Err(e) => view.set_note(format!("Giải tán nhóm thất bại: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Sends `/api/group/upgrade/community` after the second click.
    ///
    /// Does not run under `cfg(test)` or without a session. The note never includes the group id.
    pub(crate) fn upgrade_community_now(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(conv) = self.conv(id).cloned().filter(|c| c.is_group) else {
            return;
        };
        let grid = conv.thread_id;
        let lang = if self.english { "en" } else { "vi" };
        let Some(dir) = self
            .session_dir
            .clone()
            .filter(|_| self.has_session && !cfg!(test))
        else {
            self.set_note("Chưa đăng nhập — không nâng cấp cộng đồng");
            return;
        };
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.upgrade_group_to_community(&grid, lang)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match r {
                    Ok(()) => view.set_note("Đã gửi yêu cầu nâng cấp cộng đồng."),
                    Err(_) => view.set_note("Nâng cấp cộng đồng không thành."),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens "Your Profile", fetching `/api/social/profile/me` when signed in.
    pub(crate) fn open_profile_dialog(&mut self, cx: &mut Context<Self>) {
        let local_name = self
            .display_name
            .clone()
            .filter(|n| !n.is_empty())
            .or_else(|| {
                self.profiles
                    .map
                    .get(crate::profiles::ME)
                    .map(|p| p.name.clone())
                    .filter(|n| !n.is_empty())
            })
            .unwrap_or_default();
        let initial_uid = self.self_uid().unwrap_or_default().to_owned();
        let name = cx.new(|cx| {
            let mut input = LineInput::new(cx, "Tên hiển thị");
            if !local_name.is_empty() {
                input.set_text(local_name.clone(), cx);
            }
            input
        });
        let dob = cx.new(|cx| LineInput::new(cx, "dd/mm/yyyy"));
        cx.subscribe(&name, |this, _, e: &LineEvent, cx| {
            if let LineEvent::Submit(_) = e {
                this.save_profile_dialog(cx);
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(&dob, |this, _, e: &LineEvent, cx| {
            if let LineEvent::Submit(_) = e {
                this.save_profile_dialog(cx);
                cx.notify();
            }
        })
        .detach();
        let can_fetch = !cfg!(test) && self.has_session;
        self.dialog = Some(Dialog::Profile(ProfileDlg {
            me: AccountMe {
                uid: initial_uid.clone(),
                display_name: local_name,
                ..AccountMe::default()
            },
            name: name.clone(),
            dob: dob.clone(),
            gender: 0,
            loading: can_fetch,
            saving: false,
            editing: false,
            error: (!can_fetch).then(|| "Chưa đăng nhập — chỉ xem thông tin trên máy".to_owned()),
            status: None,
            cover_img: None,
            cover_busy: false,
        }));
        if !can_fetch {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let value = match api.get_me_v2().await {
                            Ok(value) => value,
                            Err(_) => api.get_me().await.map_err(|e| e.to_string())?,
                        };
                        Ok(me_from_value(&value))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let mut cover = None;
                if let Some(Dialog::Profile(d)) = view.dialog.as_mut() {
                    d.loading = false;
                    match res {
                        Ok(me) => {
                            let new_name = me.display_name.clone();
                            let new_dob = me.birthday.clone();
                            cover = Some(me.cover.clone());
                            d.gender = me.gender;
                            d.me = me;
                            if !new_name.is_empty() {
                                name.update(cx, |i, cx| i.set_text(new_name, cx));
                            }
                            if !new_dob.is_empty() {
                                dob.update(cx, |i, cx| i.set_text(new_dob, cx));
                            }
                        }
                        Err(err) => d.error = Some(err),
                    }
                }
                if let Some(cover) = cover {
                    view.load_profile_cover(cover, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_profile_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Profile(d)) = self.dialog.as_ref() else {
            return;
        };
        if d.saving || d.loading {
            return;
        }
        if !self.has_session || cfg!(test) {
            if let Some(Dialog::Profile(d)) = self.dialog.as_mut() {
                d.error = Some("Chưa đăng nhập — không thể cập nhật hồ sơ.".into());
            }
            return;
        }
        let raw_name = d.name.read(cx).text();
        let raw_dob = d.dob.read(cx).text();
        let gender = d.gender;
        let (clean_name, clean_gender, iso_dob) =
            match validate_profile_input(&raw_name, gender, &raw_dob) {
                Ok(v) => v,
                Err(msg) => {
                    if let Some(Dialog::Profile(d)) = self.dialog.as_mut() {
                        d.error = Some(msg.to_owned());
                        d.status = None;
                    }
                    return;
                }
            };
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if let Some(Dialog::Profile(d)) = self.dialog.as_mut() {
            d.saving = true;
            d.error = None;
            d.status = None;
        }
        cx.spawn(async move |this, cx| {
            let api_name = clean_name.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.update_profile(&api_name, clean_gender, &iso_dob)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(Dialog::Profile(d)) = view.dialog.as_mut() {
                    d.saving = false;
                    match res {
                        Ok(()) => {
                            d.me.display_name = clean_name.clone();
                            d.me.gender = clean_gender;
                            d.me.birthday = raw_dob.trim().to_owned();
                            d.editing = false;
                            d.status = Some("Đã cập nhật hồ sơ.".into());
                            view.display_name = Some(clean_name.clone());
                            view.profiles
                                .map
                                .entry(crate::profiles::ME.to_owned())
                                .and_modify(|p| p.name = clean_name.clone())
                                .or_insert_with(|| crate::profiles::Profile {
                                    name: clean_name.clone(),
                                    ..crate::profiles::Profile::default()
                                });
                            view.set_note("Đã cập nhật hồ sơ");
                        }
                        Err(err) => d.error = Some(format!("Cập nhật thất bại: {err}")),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_profile_cover(&mut self, url: String, cx: &mut Context<Self>) {
        if cfg!(test) || !cover_is_photo(&url) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        match self.dialog.as_mut() {
            Some(Dialog::Profile(d)) if d.cover_img.is_none() && !d.cover_busy => {
                d.cover_busy = true
            }
            _ => return,
        }
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let bytes = api.fetch_asset(&url).await.map_err(|e| e.to_string())?;
                        let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
                        let rgba = image
                            .resize(840, 480, image::imageops::FilterType::Triangle)
                            .to_rgba8();
                        Ok(crate::media::rgba_to_bgra(rgba))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(Dialog::Profile(d)) = view.dialog.as_mut() {
                    d.cover_busy = false;
                    if let Ok(rgba) = res {
                        d.cover_img = Some(Arc::new(RenderImage::new([image::Frame::new(rgba)])));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn begin_profile_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(Dialog::Profile(d)) = self.dialog.as_mut() {
            if d.loading {
                d.error = Some("Chưa tải xong hồ sơ, thử lại sau.".into());
            } else {
                d.editing = true;
                d.error = None;
                d.status = None;
            }
        }
        cx.notify();
    }

    fn cancel_profile_edit(&mut self, cx: &mut Context<Self>) {
        let copied = if let Some(Dialog::Profile(d)) = self.dialog.as_mut() {
            d.editing = false;
            d.error = None;
            d.gender = d.me.gender;
            Some((
                d.name.clone(),
                d.dob.clone(),
                d.me.display_name.clone(),
                d.me.birthday.clone(),
            ))
        } else {
            None
        };
        if let Some((name_in, dob_in, name, dob)) = copied {
            name_in.update(cx, |input, cx| input.set_text(name, cx));
            dob_in.update(cx, |input, cx| input.set_text(dob, cx));
        }
        cx.notify();
    }

    pub(crate) fn profile_el(&self, d: &ProfileDlg, cx: &mut Context<Self>) -> gpui::AnyElement {
        if d.editing {
            self.profile_edit_el(d, cx).into_any_element()
        } else {
            self.profile_view_el(d, cx).into_any_element()
        }
    }

    fn profile_view_el(&self, d: &ProfileDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let blue = token_rgb("--B55", "#0068ff");
        let danger = token_rgb("--R60", "#c31818");
        let ok_col = token_rgb("--G60", "#1f8f4e");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let paper = token_rgb("--layer-background", "#ffffff");
        let person = Person {
            uid: crate::profiles::ME.to_owned(),
            name: d.me.display_name.clone(),
            avatar: String::new(),
            note: String::new(),
        };
        let display = if d.me.display_name.is_empty() {
            "Tài khoản Zalo".to_owned()
        } else {
            d.me.display_name.clone()
        };
        // Description is `biz.desc` only. The status line is a different field and
        // Zalo PC does not print it on this card.
        let mo_ta = d.me.biz.desc.clone();
        let show_biz = d.me.biz.pkg_id != 0
            || !mo_ta.is_empty()
            || !d.me.biz.addr.is_empty()
            || !d.me.biz.website.is_empty()
            || !d.me.biz.email.is_empty();
        let phone = if d.me.phone.is_empty() {
            "—".to_owned()
        } else {
            pretty_phone(&d.me.phone)
        };
        let birthday = if d.me.birthday.is_empty() {
            "—".to_owned()
        } else {
            pretty_birthday(&d.me.birthday)
        };
        let gender = gender_word(d.me.gender).to_owned();
        let website = d.me.biz.website.clone();
        let email = d.me.biz.email.clone();
        let title = self.dialog_title_bar(str_vi("STR_ACCOUNT", "Thông tin tài khoản"), cx);
        let avatar = self.profile_avatar_badge(&person, subtle, cx);
        let notice = self.profile_notice(d, muted, danger, ok_col, blue, cx);

        div()
            .w(px(420.))
            .max_h(px(680.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .overflow_hidden()
            .bg(rgb(paper))
            .text_color(rgb(ink))
            .shadow_lg()
            .child(title)
            .child(
                div()
                    .id("profile-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(170.))
                            .overflow_hidden()
                            .bg(rgb(0x6b83e8))
                            .child(div().absolute().top_0().left_0().right_0().h(px(85.)).bg(rgb(0x8f8cf2)))
                            .when_some(d.cover_img.clone(), |el, image| {
                                el.child(
                                    img(image)
                                        .absolute()
                                        .inset_0()
                                        .size_full()
                                        .object_fit(ObjectFit::Cover),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .px(px(16.))
                            .mt(px(-40.))
                            .gap(px(12.))
                            .child(avatar)
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .pt(px(36.))
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .min_w(px(0.))
                                            .text_size(px(18.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .truncate()
                                            .child(display),
                                    )
                                    .child(
                                        div()
                                            .id("pf-rename")
                                            .flex_none()
                                            .size(px(28.))
                                            .rounded(px(14.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .tooltip(|_, cx| crate::widgets::tip("Đổi tên", cx))
                                            .hover(|st| st.bg(rgb(hover)))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.begin_profile_edit(cx);
                                            }))
                                            .child(fonts::icon_el(ic::EDIT_NAME, "✎", 16.)),
                                    ),
                            ),
                    )
                    .when(d.loading, |el| {
                        el.child(
                            div()
                                .px(px(16.))
                                .pt(px(8.))
                                .text_size(px(13.))
                                .text_color(rgb(muted))
                                .child("Đang tải hồ sơ…"),
                        )
                    })
                    .when(show_biz, |el| {
                        el.child(div().mt(px(16.)).h(px(6.)).w_full().bg(rgb(subtle))).child(
                            div()
                                .flex()
                                .flex_col()
                                .px(px(16.))
                                .pt(px(14.))
                                .pb(px(8.))
                                .gap(px(8.))
                                .child(
                                    div()
                                        .text_size(px(16.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .child(str_vi("STR_BUSINESS_INFO_HEADER", "Thông tin kinh doanh")),
                                )
                                .when(!mo_ta.is_empty(), |col| {
                                    col.child(
                                        div()
                                            .text_size(px(14.5))
                                            .text_color(rgb(muted))
                                            .child(str_vi("STR_BUSINESS_INFO_DESC", "Mô tả")),
                                    )
                                    .child(profile_desc_lines(&mo_ta, ink, blue))
                                })
                                .when(mo_ta.is_empty(), |col| {
                                    col.child(
                                        div()
                                            .text_size(px(13.5))
                                            .text_color(rgb(muted))
                                            .whitespace_normal()
                                            .child(str_vi(
                                                "STR_BUSINESS_INFO_EMPTY",
                                                "Tạo thông tin kinh doanh để khách hàng biết thêm thông tin về bạn",
                                            )),
                                    )
                                })
                                .when(!d.me.biz.addr.is_empty(), |col| {
                                    col.child(profile_kv(
                                        str_vi("STR_BUSINESS_INFO_ADDR", "Địa chỉ"),
                                        &d.me.biz.addr,
                                        ink,
                                        muted,
                                    ))
                                })
                                .when(!website.is_empty(), |col| {
                                    let url = website.clone();
                                    col.child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .h(px(28.))
                                            .child(
                                                div()
                                                    .w(px(108.))
                                                    .flex_none()
                                                    .text_size(px(14.5))
                                                    .text_color(rgb(muted))
                                                    .child(str_vi("STR_BUSINESS_INFO_URL", "Website")),
                                            )
                                            .child(
                                                div()
                                                    .id("pf-website")
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .text_size(px(14.5))
                                                    .text_color(rgb(blue))
                                                    .truncate()
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |_, _, _, cx| {
                                                        cx.open_url(&url);
                                                    }))
                                                    .child(website.clone()),
                                            ),
                                    )
                                })
                                .when(!email.is_empty(), |col| {
                                    col.child(profile_kv(
                                        str_vi("STR_BUSINESS_INFO_EMAIL", "Email"),
                                        &email,
                                        ink,
                                        muted,
                                    ))
                                })
                                .child(
                                    div()
                                        .id("pf-biz-update")
                                        .h(px(40.))
                                        .w_full()
                                        .mt(px(4.))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .justify_center()
                                        .gap(px(6.))
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .text_color(rgb(blue))
                                        .text_size(px(14.5))
                                        .hover(|st| st.bg(rgb(hover)))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.open_url("https://business.zbox.vn/");
                                            this.set_note(
                                                "Máy tính không có API sửa hồ sơ doanh nghiệp. Đã mở trang Zalo Business.",
                                            );
                                            cx.notify();
                                        }))
                                        .child(fonts::icon_el(ic::EDIT_NAME, "✎", 16.))
                                        .child(str_vi("STR_UPDATE", "Cập nhật")),
                                ),
                        )
                    })
                    .child(div().mt(px(8.)).h(px(6.)).w_full().bg(rgb(subtle)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .px(px(16.))
                            .pt(px(14.))
                            .pb(px(12.))
                            .gap(px(2.))
                            .child(
                                div()
                                    .pb(px(8.))
                                    .text_size(px(16.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .child(str_vi("STR_PERSONAL_INFO", "Thông tin cá nhân")),
                            )
                            .child(profile_kv(
                                str_vi("STR_PROFILE_LABEL_GENDER", "Giới tính"),
                                &gender,
                                ink,
                                muted,
                            ))
                            .child(profile_kv(
                                str_vi("STR_PROFILE_LABEL_BIRTHDAY", "Ngày sinh"),
                                &birthday,
                                ink,
                                muted,
                            ))
                            .child(profile_kv(
                                str_vi("STR_PROFILE_PHONENUMBER", "Điện thoại"),
                                &phone,
                                ink,
                                muted,
                            ))
                            .child(
                                div()
                                    .pt(px(6.))
                                    .text_size(px(13.))
                                    .line_height(px(18.))
                                    .text_color(rgb(muted))
                                    .whitespace_normal()
                                    .child(str_vi(
                                        "STR_PHONE_NUMBER_EXTRA_INFO",
                                        "Chỉ bạn bè có lưu số của bạn trong danh bạ máy xem được số này",
                                    )),
                            )
                            .child(
                                div()
                                    .id("pf-me-update")
                                    .h(px(40.))
                                    .w_full()
                                    .mt(px(8.))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(6.))
                                    .rounded(px(4.))
                                    .cursor_pointer()
                                    .text_color(rgb(blue))
                                    .text_size(px(14.5))
                                    .hover(|st| st.bg(rgb(hover)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.begin_profile_edit(cx);
                                    }))
                                    .child(fonts::icon_el(ic::EDIT_NAME, "✎", 16.))
                                    .child(str_vi("STR_UPDATE", "Cập nhật")),
                            ),
                    )
                    .child(notice),
            )
    }

    fn profile_avatar_badge(
        &self,
        person: &Person,
        subtle: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("pf-avatar")
            .relative()
            .flex_none()
            .size(px(80.))
            .cursor_pointer()
            .tooltip(|_, cx| crate::widgets::tip("Đổi ảnh đại diện", cx))
            .on_click(cx.listener(|this, _, _, cx| {
                this.pick_profile_avatar(cx);
            }))
            .child(
                div()
                    .size(px(80.))
                    .rounded(px(40.))
                    .border_2()
                    .border_color(rgb(0xffffff))
                    .overflow_hidden()
                    .bg(rgb(0xffffff))
                    .child(self.dialog_avatar(person, 76.)),
            )
            .child(
                div()
                    .absolute()
                    .right(px(0.))
                    .bottom(px(0.))
                    .size(px(24.))
                    .rounded(px(12.))
                    .bg(rgb(subtle))
                    .border_2()
                    .border_color(rgb(0xffffff))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(fonts::icon_el('\u{EA4A}', "📷", 12.)),
            )
    }

    fn profile_notice(
        &self,
        d: &ProfileDlg,
        muted: u32,
        danger: u32,
        ok_col: u32,
        blue: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expired = d.error.as_deref().is_some_and(|err| {
            err.contains("hết hạn") || err.contains("Chưa đăng nhập") || err.contains("session")
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(16.))
            .pb(px(12.))
            .when_some(d.error.clone(), |el, err| {
                el.child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(if expired { muted } else { danger }))
                        .whitespace_normal()
                        .child(if expired {
                            "Phiên đăng nhập đã hết hạn. Quét QR để cập nhật hồ sơ trên máy chủ."
                                .to_owned()
                        } else {
                            err
                        }),
                )
            })
            .when(expired, |el| {
                el.child(
                    div()
                        .id("pf-relogin")
                        .h(px(32.))
                        .px(px(12.))
                        .rounded(px(4.))
                        .bg(rgb(blue))
                        .text_color(rgb(0xffffff))
                        .text_size(px(13.5))
                        .font_family(FAMILY_SEMIBOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.leave_to_login(window, cx, false);
                        }))
                        .child("Đăng nhập lại"),
                )
            })
            .when_some(d.status.clone(), |el, st| {
                el.child(div().text_size(px(13.)).text_color(rgb(ok_col)).child(st))
            })
    }

    fn profile_edit_el(&self, d: &ProfileDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let blue = token_rgb("--B55", "#0068ff");
        let danger = token_rgb("--R60", "#c31818");
        let paper = token_rgb("--layer-background", "#ffffff");
        let can_save = self.has_session && !d.loading && !d.saving;
        let gender_chip =
            |id: &'static str, code: i64, label: &'static str, cx: &mut Context<Self>| {
                let active = d.gender == code;
                div()
                    .id(id)
                    .h(px(32.))
                    .px(px(14.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(13.5))
                    .bg(if active { rgb(blue) } else { rgb(subtle) })
                    .text_color(rgb(if active { 0xffffff } else { ink }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(Dialog::Profile(dlg)) = this.dialog.as_mut() {
                            dlg.gender = code;
                        }
                        cx.notify();
                    }))
                    .child(label)
            };
        div()
            .w(px(420.))
            .h(px(480.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .overflow_hidden()
            .bg(rgb(paper))
            .text_color(rgb(ink))
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .h(px(52.))
                    .px(px(8.))
                    .gap(px(4.))
                    .border_b_1()
                    .border_color(crate::color::border_subtle())
                    .child(
                        div()
                            .id("pf-back")
                            .size(px(32.))
                            .rounded(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|st| st.bg(rgb(subtle)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_profile_edit(cx);
                            }))
                            .child(fonts::icon_el('\u{EA94}', "‹", 18.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(16.))
                            .font_family(FAMILY_SEMIBOLD)
                            .truncate()
                            .child(str_vi(
                                "STR_UPDATE_PERSONAL_INFO",
                                "Cập nhật thông tin cá nhân",
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(16.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child(str_vi("STR_DISPLAY_NAME", "Tên hiển thị")),
                    )
                    .child(
                        div()
                            .h(px(36.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .child(d.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child(str_vi("STR_PROFILE_UPDATE_LABEL_GENDER", "Giới tính")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .child(gender_chip(
                                "pf-male",
                                0,
                                str_vi("STR_GENDER_MALE", "Nam"),
                                cx,
                            ))
                            .child(gender_chip(
                                "pf-female",
                                1,
                                str_vi("STR_GENDER_FEMALE", "Nữ"),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child(str_vi("STR_PROFILE_LABEL_BIRTHDAY", "Ngày sinh")),
                    )
                    .child(
                        div()
                            .h(px(36.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .child(d.dob.clone()),
                    )
                    .when_some(d.error.clone(), |el, err| {
                        el.child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(danger))
                                .whitespace_normal()
                                .child(err),
                        )
                    }),
            )
            .child(dialog_footer(
                str_vi("STR_CANCEL", "Hủy"),
                if d.saving {
                    "Đang lưu…"
                } else {
                    str_vi("STR_UPDATE", "Cập nhật")
                },
                can_save,
                cx.listener(|this, _, _, cx| {
                    this.cancel_profile_edit(cx);
                }),
                cx.listener(|this, _, _, cx| {
                    this.save_profile_dialog(cx);
                    cx.notify();
                }),
            ))
    }

    fn pick_profile_avatar(&mut self, cx: &mut Context<Self>) {
        self.pick_avatar_for(None, cx);
    }

    /// `None` updates this account (`/api/profile/upavatar`). `Some` is a
    /// group thread id (`/api/group/upavatar`).
    pub(crate) fn pick_avatar_for(&mut self, group: Option<String>, cx: &mut Context<Self>) {
        if self.presence.avatar_busy {
            return;
        }
        if cfg!(test) || !self.has_session {
            self.set_note("Chưa đăng nhập — không thể đổi ảnh đại diện.");
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            self.set_note("Chưa đăng nhập — không thể đổi ảnh đại diện.");
            return;
        };
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move { crate::plugins::pick_photo() })
                .await;
            let path = match picked {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(err) => {
                    let _ = this.update(cx, |view, cx| {
                        view.set_note(err);
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |view, cx| {
                view.presence.avatar_busy = true;
                view.set_note("Đang đổi ảnh đại diện…");
                cx.notify();
            });
            let path_decode = path.clone();
            let group_key = group.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let uploaded = crate::bridge::block_api(&dir, |api| {
                        let path = path.clone();
                        let group = group.clone();
                        async move {
                            crate::presence::upload_avatar_file(&api, &path, group.as_deref()).await
                        }
                    });
                    uploaded.map(|url| (url, crate::media::decode_thumb(&path_decode, 96)))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.presence.avatar_busy = false;
                match res {
                    Ok((url, image)) => {
                        let key = group_key.unwrap_or_else(|| crate::profiles::ME.to_owned());
                        if let Some(image) = image {
                            view.avatars.insert(key.clone(), image);
                        } else if url.starts_with("http://") || url.starts_with("https://") {
                            view.avatars.remove(&key);
                            view.avatars_queued = true;
                        }
                        if url.starts_with("http://") || url.starts_with("https://") {
                            view.profiles
                                .map
                                .entry(key)
                                .and_modify(|p| p.avatar = url.clone())
                                .or_insert_with(|| crate::profiles::Profile {
                                    avatar: url,
                                    ..crate::profiles::Profile::default()
                                });
                            if let Some(path) = crate::profiles::cache_path() {
                                view.profiles.save(&path);
                            }
                        }
                        view.set_note("Đổi ảnh đại diện thành công");
                    }
                    Err(err) => view.set_note(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the `.zdb4` backup decryption modal.
    pub(crate) fn open_backup_decrypt(&mut self, cx: &mut Context<Self>) {
        let path = cx.new(|cx| LineInput::new(cx, "Đường dẫn tệp .zdb4"));
        let key = cx.new(|cx| LineInput::new_masked(cx, "Khóa giải mã ZDB4"));
        cx.subscribe(&path, |this, _, e: &LineEvent, cx| {
            if let LineEvent::Submit(_) = e {
                this.submit_backup_decrypt(cx);
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(&key, |this, _, e: &LineEvent, cx| {
            if let LineEvent::Submit(_) = e {
                this.submit_backup_decrypt(cx);
                cx.notify();
            }
        })
        .detach();
        self.dialog = Some(Dialog::BackupDecrypt(BackupDecryptDlg {
            path,
            key,
            out_dir: default_zdb4_out_dir(),
            busy: false,
            result: None,
        }));
    }

    fn pick_zdb4_file(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::BackupDecrypt(d)) = self.dialog.as_ref() else {
            return;
        };
        let path_ent = d.path.clone();
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move { zalo_shell::pick_file("Chọn tệp sao lưu .zdb4") })
                .await;
            let _ = this.update(cx, |view, cx| {
                match picked {
                    Ok(Some(file)) => {
                        path_ent.update(cx, |i, cx| i.set_text(file.display().to_string(), cx));
                    }
                    Ok(None) => {}
                    Err(err) => {
                        if let Some(Dialog::BackupDecrypt(d)) = view.dialog.as_mut() {
                            d.result = Some(Err(err.to_string()));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_backup_decrypt(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::BackupDecrypt(d)) = self.dialog.as_ref() else {
            return;
        };
        if d.busy {
            return;
        }
        let path_text = d.path.read(cx).text();
        let key_text = d.key.read(cx).text();
        let out_dir = d.out_dir.clone();
        if path_text.trim().is_empty() || key_text.trim().is_empty() {
            if let Some(Dialog::BackupDecrypt(d)) = self.dialog.as_mut() {
                d.result = Some(Err("Cần nhập đường dẫn tệp .zdb4 và khóa giải mã.".into()));
            }
            return;
        }
        if let Some(Dialog::BackupDecrypt(d)) = self.dialog.as_mut() {
            d.busy = true;
            d.result = None;
        }
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    decrypt_zdb4_to_dir(Path::new(path_text.trim()), &key_text, &out_dir)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(Dialog::BackupDecrypt(d)) = view.dialog.as_mut() {
                    d.busy = false;
                    match res {
                        Ok((count, summary)) => {
                            let line = format!("Đã giải mã {count} tệp: {}", summary.join(", "));
                            d.result = Some(Ok(line));
                            view.set_note(format!("Đã giải mã {count} tệp .zdb4"));
                        }
                        Err(err) => d.result = Some(Err(err)),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn backup_decrypt_el(
        &self,
        d: &BackupDecryptDlg,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let danger = token_rgb("--R60", "#c31818");
        let ok_col = token_rgb("--G60", "#1f8f4e");
        let ok_dir = d.out_dir.clone();
        let show_open = matches!(d.result, Some(Ok(_)));

        div()
            .w(px(480.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .text_color(rgb(ink))
            .shadow_lg()
            .child(self.dialog_title_bar("Giải mã bản sao lưu .zdb4", cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .p(px(16.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(muted))
                                    .child("Tệp sao lưu .zdb4"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .h(px(36.))
                                            .px(px(10.))
                                            .flex()
                                            .items_center()
                                            .rounded(px(4.))
                                            .border_1()
                                            .border_color(crate::color::border_subtle())
                                            .child(d.path.clone()),
                                    )
                                    .child(
                                        div()
                                            .id("zdb4-pick")
                                            .h(px(36.))
                                            .px(px(12.))
                                            .rounded(px(4.))
                                            .bg(rgb(subtle))
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_size(px(13.))
                                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                this.pick_zdb4_file(cx);
                                            }))
                                            .child("Chọn tệp…"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(muted))
                                    .child("Khóa giải mã / private key"),
                            )
                            .child(
                                div()
                                    .h(px(36.))
                                    .px(px(10.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(crate::color::border_subtle())
                                    .child(d.key.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(muted))
                            .child(format!("Thư mục xuất: {}", d.out_dir.display())),
                    )
                    .when_some(d.result.clone(), |el, res| match res {
                        Ok(msg) => el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .p(px(10.))
                                .rounded(px(6.))
                                .bg(rgb(subtle))
                                .child(div().text_size(px(13.)).text_color(rgb(ok_col)).child(msg))
                                .when(show_open, |el| {
                                    let open_dir = ok_dir.clone();
                                    el.child(
                                        div()
                                            .id("zdb4-open-dir")
                                            .px(px(10.))
                                            .py(px(6.))
                                            .rounded(px(4.))
                                            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
                                            .border_1()
                                            .border_color(crate::color::border_subtle())
                                            .cursor_pointer()
                                            .text_size(px(13.))
                                            .on_click(cx.listener(move |_, _, _, _| {
                                                let _ = std::process::Command::new("xdg-open")
                                                    .arg(&open_dir)
                                                    .spawn();
                                            }))
                                            .child("Mở thư mục giải mã"),
                                    )
                                }),
                        ),
                        Err(err) => {
                            el.child(div().text_size(px(13.)).text_color(rgb(danger)).child(err))
                        }
                    }),
            )
            .child(dialog_footer(
                "Đóng",
                if d.busy {
                    "Đang giải mã…"
                } else {
                    "Giải mã"
                },
                !d.busy,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.submit_backup_decrypt(cx);
                    cx.notify();
                }),
            ))
    }

    fn group_manage_advanced(
        &self,
        d: &GroupManageDlg,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--blue", "#0068ff");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let card = token_rgb("--layer-background", "#ffffff");
        let danger = token_rgb("--danger", "#e53935");
        let mine = self.self_member_ids();
        let can_manage = group_role_label(&mine, &d.creator, &d.admins) != "Thành viên";
        let conv_id = d.conv_id;
        let pending_label = if d.pending.is_empty() {
            "Duyệt thành viên".to_owned()
        } else {
            format!("Duyệt ({})", d.pending.len())
        };
        let tabs: [(GroupManageTab, String, &'static str); 3] = [
            (
                GroupManageTab::Permissions,
                "Quyền nhóm".to_owned(),
                "gm-tab-perm",
            ),
            (GroupManageTab::Pending, pending_label, "gm-tab-pending"),
            (
                GroupManageTab::Link,
                "Liên kết nhóm".to_owned(),
                "gm-tab-link",
            ),
        ];

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.))
                    .children(tabs.into_iter().map(|(tab, label, id)| {
                        let active = d.tab == tab;
                        div()
                            .id(id)
                            .px(px(10.))
                            .py(px(5.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .bg(rgb(if active { blue } else { subtle }))
                            .text_size(px(12.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(if active { 0xff_ffff } else { ink }))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut() {
                                    dlg.tab = tab;
                                    dlg.error = None;
                                    dlg.tab_note = None;
                                }
                                if tab == GroupManageTab::Pending {
                                    this.load_group_pending(conv_id, cx);
                                } else if tab == GroupManageTab::Link {
                                    this.load_group_link(conv_id, cx);
                                }
                                cx.notify();
                            }))
                            .child(label)
                    })),
            )
            .child(
                div()
                    .id("gm-tab-scroll")
                    .p(px(10.))
                    .rounded(px(6.))
                    .bg(rgb(subtle))
                    .border_1()
                    .border_color(crate::color::border_subtle())
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(match d.tab {
                        GroupManageTab::Permissions => div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .when(!can_manage && !d.loading, |el| {
                                el.child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(rgb(muted))
                                        .child("Chỉ trưởng hoặc phó nhóm mới có thể thay đổi các quyền này."),
                                )
                            })
                            .children(GROUP_SETTING_ROWS.iter().enumerate().map(|(i, (key, label_key, hint_key))| {
                                let on = d.settings.get(*key);
                                let key = *key;
                                let label = match *label_key {
                                    "STR_TOAST_ADMIN_RENAME" => setting_copy(*label_key, "Chỉ trưởng/phó nhóm được phép đổi tên nhóm"),
                                    "STR_MARK_ADMIN_MESSAGE" => setting_copy(*label_key, "Đánh dấu tin nhắn từ trưởng/phó nhóm"),
                                    "STE_ADMIN_CREATE_TOPIC" => {
                                        setting_copy(*label_key, "Chỉ trưởng/phó nhóm được phép tạo ghi chú, nhắc hẹn")
                                    }
                                    "STE_ADMIN_CREATE_POLL" => setting_copy(*label_key, "Chỉ trưởng/phó nhóm được phép tạo bình chọn"),
                                    "STR_ADMIN_MEMBERSHIP_APPROVAL" => setting_copy(*label_key, "Chế độ phê duyệt thành viên mới"),
                                    "STR_LOCK_SEND_MESSAGE_MESSAGE_FULL" => {
                                        setting_copy(*label_key, "Chỉ trưởng/phó nhóm được gửi tin nhắn vào nhóm này.")
                                    }
                                    _ => setting_copy(
                                        *label_key,
                                        "Chỉ trưởng/phó nhóm xem được đầy đủ danh sách thành viên.",
                                    ),
                                };
                                let hint = if hint_key.is_empty() {
                                    String::new()
                                } else {
                                    setting_copy(*hint_key, "Chỉ trưởng/phó nhóm được phép thay ảnh đại diện")
                                };
                                div()
                                    .id(("gm-set", i as u64))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.))
                                    .py(px(4.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.))
                                            .child(div().text_size(px(13.)).text_color(rgb(ink)).child(label))
                                            .when(!hint.is_empty(), |el| {
                                                el.child(div().text_size(px(11.5)).text_color(rgb(muted)).child(hint))
                                            }),
                                    )
                                    .child(
                                        div()
                                            .id(("gm-set-btn", i as u64))
                                            .flex_none()
                                            .when(can_manage && !d.settings_busy, |el| {
                                                el.cursor_pointer().on_click(cx.listener(
                                                    move |this, _: &ClickEvent, _, cx| {
                                                        this.toggle_group_setting(conv_id, key, !on, cx);
                                                    },
                                                ))
                                            })
                                            .child(crate::info::switch_pill(on)),
                                    )
                            }))
                            .into_any_element(),
                        GroupManageTab::Pending => {
                            if d.pending_loading {
                                div().text_size(px(12.)).text_color(rgb(muted)).child("Đang tải yêu cầu tham gia…").into_any_element()
                            } else if !can_manage {
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(muted))
                                    .child("Chỉ trưởng hoặc phó nhóm mới xem được yêu cầu tham gia.")
                                    .into_any_element()
                            } else if let Some(note) = d.tab_note.clone() {
                                div().text_size(px(12.)).text_color(rgb(danger)).child(note).into_any_element()
                            } else if d.pending.is_empty() {
                                div().text_size(px(12.)).text_color(rgb(muted)).child("Không có yêu cầu chờ duyệt.").into_any_element()
                            } else {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.))
                                    .children(d.pending.iter().enumerate().map(|(i, m)| {
                                        let uid_ok = m.uid.clone();
                                        let uid_no = m.uid.clone();
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(8.))
                                            .child(div().text_size(px(12.5)).text_color(rgb(ink)).child(m.name.clone()))
                                            .when(can_manage, |el| {
                                                el.child(
                                                    div()
                                                        .flex()
                                                        .flex_row()
                                                        .gap(px(6.))
                                                        .child(
                                                            div()
                                                                .id(("gm-pend-ok", i as u64))
                                                                .px(px(8.))
                                                                .py(px(3.))
                                                                .rounded(px(4.))
                                                                .bg(rgb(blue))
                                                                .text_size(px(11.5))
                                                                .text_color(rgb(0xff_ffff))
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                                    this.review_group_pending(conv_id, uid_ok.clone(), true, cx);
                                                                }))
                                                                .child("Duyệt"),
                                                        )
                                                        .child(
                                                            div()
                                                                .id(("gm-pend-no", i as u64))
                                                                .px(px(8.))
                                                                .py(px(3.))
                                                                .rounded(px(4.))
                                                                .bg(rgb(card))
                                                                .border_1()
                                                                .border_color(crate::color::border_subtle())
                                                                .text_size(px(11.5))
                                                                .text_color(rgb(danger))
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                                    this.review_group_pending(conv_id, uid_no.clone(), false, cx);
                                                                }))
                                                                .child("Từ chối"),
                                                        ),
                                                )
                                            })
                                    }))
                                    .into_any_element()
                            }
                        }
                        GroupManageTab::Link => {
                            let url = d.link.url.clone();
                            let url_copy = url.clone();
                            let link_note = d.tab_note.clone();
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .when_some(link_note, |el, note| {
                                    el.child(div().text_size(px(12.)).text_color(rgb(danger)).child(note))
                                })
                                .child(div().text_size(px(12.)).text_color(rgb(muted)).child(if d.link_loading {
                                    "Đang tải liên kết tham gia…".to_owned()
                                } else if d.link.enabled && !url.is_empty() {
                                    url.clone()
                                } else {
                                    "Liên kết tham gia nhóm đang tắt.".to_owned()
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .flex_wrap()
                                        .gap(px(6.))
                                        .when(!url.is_empty() && d.link.enabled, |el| {
                                            el.child(
                                                div()
                                                    .id("gm-link-copy")
                                                    .px(px(10.))
                                                    .py(px(4.))
                                                    .rounded(px(4.))
                                                    .bg(rgb(card))
                                                    .border_1()
                                                    .border_color(crate::color::border_subtle())
                                                    .text_size(px(12.))
                                                    .text_color(rgb(ink))
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        cx.write_to_clipboard(ClipboardItem::new_string(url_copy.clone()));
                                                        this.set_note("Đã sao chép liên kết nhóm");
                                                        cx.notify();
                                                    }))
                                                    .child("Sao chép link"),
                                            )
                                        })
                                        .when(can_manage && !d.link_loading, |el| {
                                            el.child(
                                                div()
                                                    .id("gm-link-renew")
                                                    .px(px(10.))
                                                    .py(px(4.))
                                                    .rounded(px(4.))
                                                    .bg(rgb(blue))
                                                    .text_size(px(12.))
                                                    .text_color(rgb(0xff_ffff))
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.mutate_group_link(conv_id, true, cx);
                                                    }))
                                                    .child(if d.link.enabled { "Đặt lại link" } else { "Bật / tạo link mới" }),
                                            )
                                            .when(d.link.enabled, |el| {
                                                el.child(
                                                    div()
                                                        .id("gm-link-disable")
                                                        .px(px(10.))
                                                        .py(px(4.))
                                                        .rounded(px(4.))
                                                        .bg(rgb(card))
                                                        .border_1()
                                                        .border_color(crate::color::border_subtle())
                                                        .text_size(px(12.))
                                                        .text_color(rgb(danger))
                                                        .cursor_pointer()
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            this.mutate_group_link(conv_id, false, cx);
                                                        }))
                                                        .child("Tắt link"),
                                                )
                                            })
                                        }),
                                )
                                .into_any_element()
                        }
                    }),
            )
    }

    fn toggle_group_setting(
        &mut self,
        conv_id: u64,
        key: GroupSettingKey,
        next: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((grid, prev, updated, patch)) = self.dialog.as_ref().and_then(|dlg| match dlg {
            Dialog::GroupManage(d) if d.conv_id == conv_id && !d.settings_busy => {
                let (updated, patch) = d.settings.with_change(key, next);
                Some((d.thread_id.clone(), d.settings.clone(), updated, patch))
            }
            _ => None,
        }) else {
            return;
        };
        if cfg!(test) || !self.has_session || !crate::data::live_allowed(&grid) {
            self.set_note("Chưa đăng nhập — không thể cập nhật cài đặt nhóm");
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if let Some(Dialog::GroupManage(d)) = self.dialog.as_mut() {
            d.settings = updated;
            d.settings_busy = true;
            d.error = None;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.update_group_settings(&grid, &patch)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(Dialog::GroupManage(d)) = view.dialog.as_mut() {
                    if d.conv_id == conv_id {
                        d.settings_busy = false;
                        if let Err(err) = res {
                            d.settings = prev;
                            d.error = Some(err);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_group_pending(&mut self, conv_id: u64, cx: &mut Context<Self>) {
        let Some((grid, admin)) = self.dialog.as_ref().and_then(|dlg| match dlg {
            Dialog::GroupManage(d) if d.conv_id == conv_id => Some((
                d.thread_id.clone(),
                group_role_label(&self.self_member_ids(), &d.creator, &d.admins) != "Thành viên",
            )),
            _ => None,
        }) else {
            return;
        };
        if !admin {
            if let Some(Dialog::GroupManage(d)) = self.dialog.as_mut() {
                d.pending_loading = false;
            }
            return;
        }
        if cfg!(test) || !self.has_session || !crate::data::live_allowed(&grid) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if let Some(Dialog::GroupManage(d)) = self.dialog.as_mut() {
            d.pending_loading = true;
        }
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.get_group_pending_members(&grid)
                            .await
                            .map(|v| group_pending_from(&v))
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let synced = {
                    let mut n = None;
                    if let Some(Dialog::GroupManage(d)) = view.dialog.as_mut() {
                        if d.conv_id == conv_id {
                            d.pending_loading = false;
                            match res {
                                Ok(list) => {
                                    n = Some(list.len());
                                    d.pending = list;
                                    d.tab_note = None;
                                }
                                Err(err) => d.tab_note = Some(group_side_note(&err)),
                            }
                        }
                    }
                    n
                };
                if let Some(n) = synced {
                    view.group_pending_n.insert(conv_id, n);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn review_group_pending(
        &mut self,
        conv_id: u64,
        uid: String,
        approve: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.dialog.as_ref().and_then(|dlg| match dlg {
            Dialog::GroupManage(d) if d.conv_id == conv_id => Some(d.thread_id.clone()),
            _ => None,
        }) else {
            return;
        };
        if cfg!(test) || !self.has_session || !crate::data::live_allowed(&grid) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let target = uid.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.review_group_pending_members(&grid, &[target], approve)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let synced = {
                    let mut n = None;
                    if let Some(Dialog::GroupManage(d)) = view.dialog.as_mut() {
                        if d.conv_id == conv_id {
                            match res {
                                Ok(()) => {
                                    d.pending.retain(|m| m.uid != uid);
                                    if approve {
                                        d.member_count += 1;
                                    }
                                    n = Some(d.pending.len());
                                }
                                Err(err) => d.error = Some(err),
                            }
                        }
                    }
                    n
                };
                if let Some(n) = synced {
                    view.group_pending_n.insert(conv_id, n);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_group_link(&mut self, conv_id: u64, cx: &mut Context<Self>) {
        let Some(grid) = self.dialog.as_ref().and_then(|dlg| match dlg {
            Dialog::GroupManage(d) if d.conv_id == conv_id => Some(d.thread_id.clone()),
            _ => None,
        }) else {
            return;
        };
        if cfg!(test) || !self.has_session || !crate::data::live_allowed(&grid) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if let Some(Dialog::GroupManage(d)) = self.dialog.as_mut() {
            d.link_loading = true;
        }
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.get_group_link_detail(&grid)
                            .await
                            .map(|v| group_link_from(&v))
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(Dialog::GroupManage(d)) = view.dialog.as_mut() {
                    if d.conv_id == conv_id {
                        d.link_loading = false;
                        match res {
                            Ok(link) => {
                                d.link = link;
                                d.tab_note = None;
                            }
                            Err(err) => d.tab_note = Some(group_side_note(&err)),
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn mutate_group_link(&mut self, conv_id: u64, renew: bool, cx: &mut Context<Self>) {
        let Some(grid) = self.dialog.as_ref().and_then(|dlg| match dlg {
            Dialog::GroupManage(d) if d.conv_id == conv_id => Some(d.thread_id.clone()),
            _ => None,
        }) else {
            return;
        };
        if cfg!(test) || !self.has_session || !crate::data::live_allowed(&grid) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if let Some(Dialog::GroupManage(d)) = self.dialog.as_mut() {
            d.link_loading = true;
            d.error = None;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        if renew {
                            api.renew_group_link(&grid)
                                .await
                                .map(|v| group_link_from(&v))
                                .map_err(|e| e.to_string())
                        } else {
                            api.disable_group_link(&grid)
                                .await
                                .map(|_| GroupLinkState {
                                    url: String::new(),
                                    enabled: false,
                                })
                                .map_err(|e| e.to_string())
                        }
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(Dialog::GroupManage(d)) = view.dialog.as_mut() {
                    if d.conv_id == conv_id {
                        d.link_loading = false;
                        match res {
                            Ok(link) => d.link = link,
                            Err(err) => d.error = Some(err),
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Pending/link failures stay on that tab. Code 114 is Zalo's invalid-parameter
/// reply when a regular member asks for the approval list.
fn group_side_note(err: &str) -> String {
    if err.contains("114") {
        "Zalo không nhận yêu cầu này với vai trò hiện tại.".to_owned()
    } else {
        err.to_owned()
    }
}

fn profile_kv(label: &str, value: &str, ink: u32, muted: u32) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .min_h(px(28.))
        .child(
            div()
                .w(px(108.))
                .flex_none()
                .text_size(px(14.5))
                .text_color(rgb(muted))
                .child(label.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(14.5))
                .text_color(rgb(ink))
                .whitespace_normal()
                .child(value.to_owned()),
        )
}

fn profile_desc_lines(text: &str, ink: u32, blue: u32) -> impl IntoElement {
    let mut col = div().flex().flex_col().gap(px(2.));
    let mut any = false;
    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        any = true;
        let digits: String = line.chars().filter(|c| c.is_ascii_digit()).collect();
        let phone = (9..=11).contains(&digits.len());
        col = col.child(
            div()
                .text_size(px(14.5))
                .line_height(px(20.))
                .text_color(rgb(if phone { blue } else { ink }))
                .whitespace_normal()
                .child(line.to_owned()),
        );
    }
    if !any {
        col = col.child(
            div()
                .text_size(px(14.5))
                .text_color(rgb(ink))
                .whitespace_normal()
                .child(text.to_owned()),
        );
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_me_info_and_validates_profile_updates() {
        let raw = serde_json::json!({
            "data": {
                "userId": "100200300",
                "displayName": "Nguyễn Văn A",
                "zaloName": "van.a",
                "gender": 1,
                "sdob": "1992-07-05",
                "phoneNumber": "84901234567",
                "status": "Sẵn sàng"
            }
        });
        let me = me_from_value(&raw);
        assert_eq!(me.uid, "100200300");
        assert_eq!(me.display_name, "Nguyễn Văn A");
        assert_eq!(me.gender, 1);
        assert_eq!(me.birthday, "05/07/1992");
        assert_eq!(me.phone, "84901234567");
        assert!(me.biz.desc.is_empty());
        assert_eq!(pretty_birthday("21/04/1988"), "21 tháng 04, 1988");
        assert_eq!(pretty_phone("84901234567"), "+84 901 234 567");
        assert_eq!(pretty_phone("+84901234567"), "+84 901 234 567");
        assert!(!cover_is_photo("https://res.zalo.me/default"));
        assert!(cover_is_photo("https://cdn.example/cover.jpg"));

        let biz = me_from_value(&serde_json::json!({
            "data": {
                "userId": "1",
                "displayName": "A",
                "gender": 0,
                "bizPkg": { "pkgId": 0 },
                "bizInfo": {
                    "pkgId": 2,
                    "biz": {
                        "desc": "PC chính hãng",
                        "addr": "Q1",
                        "website": "https://example.com",
                        "email": "a@example.com"
                    }
                },
                "cover": { "url": "https://cdn.example/cover.jpg" }
            }
        }));
        assert_eq!(biz.biz.pkg_id, 2);
        assert_eq!(biz.biz.desc, "PC chính hãng");
        assert_eq!(biz.biz.addr, "Q1");
        assert_eq!(biz.biz.website, "https://example.com");
        assert_eq!(biz.cover, "https://cdn.example/cover.jpg");

        let v2 = me_from_value(&serde_json::json!({
            "profile": {
                "userId": "9",
                "displayName": "B",
                "gender": "0",
                "status": "bio",
                "bizPkg": { "pkgId": "3" },
                "cover": "https://cdn.example/cover.jpg"
            },
            "biz": { "desc": "Dòng 1\nCall: 0790000000", "addr": "Q1" }
        }));
        assert_eq!(v2.display_name, "B");
        assert_eq!(v2.biz.pkg_id, 3);
        assert_eq!(v2.biz.desc, "Dòng 1\nCall: 0790000000");
        assert_eq!(v2.biz.addr, "Q1");
        assert_eq!(v2.status, "bio");
        assert_eq!(v2.gender, 0);

        let packed = me_from_value(&serde_json::json!({
            "bizInfo": { "pkgId": "4", "biz": "{\"desc\":\"Từ chuỗi\",\"email\":\"a@b.c\"}" }
        }));
        assert_eq!(packed.biz.pkg_id, 4);
        assert_eq!(packed.biz.desc, "Từ chuỗi");
        assert_eq!(packed.biz.email, "a@b.c");

        assert_eq!(iso_birthday("05/07/1992").as_deref(), Some("1992-07-05"));
        assert_eq!(iso_birthday("1992-07-05").as_deref(), Some("1992-07-05"));
        assert_eq!(iso_birthday("99/99/1992"), None);
        assert!(validate_profile_input("  ", 0, "05/07/1992").is_err());
        assert_eq!(
            validate_profile_input("  An  ", 99, "05/07/1992").unwrap(),
            ("An".to_owned(), 0, "1992-07-05".to_owned())
        );
    }

    #[test]
    fn group_roles_and_zdb4_safety_checks() {
        let mine: HashSet<String> = ["me".into()].into_iter().collect();
        assert_eq!(group_role_label(&mine, "me", &[]), "Trưởng nhóm");
        assert_eq!(group_role_label(&mine, "owner", &["me".into()]), "Phó nhóm");
        assert_eq!(
            group_role_label(&mine, "owner", &["other".into()]),
            "Thành viên"
        );

        assert_eq!(
            safe_zdb4_entry_name("dir\\sub/msg.db").as_deref(),
            Some("msg.db")
        );
        assert_eq!(safe_zdb4_entry_name("../../.."), None);
        assert_eq!(safe_zdb4_entry_name(""), None);

        let err = decrypt_zdb4_to_dir(Path::new(""), "key", Path::new("/tmp/out")).unwrap_err();
        assert!(err.contains("khóa giải mã"));
    }

    #[test]
    fn parses_group_settings_pending_and_link() {
        let info = serde_json::json!({
            "data": {
                "gridInfoMap": {
                    "901": {
                        "setting": {
                            "blockName": 1,
                            "signAdminMsg": 1,
                            "lockCreatePost": 0,
                            "lockCreatePoll": 1,
                            "joinAppr": 1,
                            "lockSendMsg": 0,
                            "lockViewMember": 1
                        }
                    }
                }
            }
        });
        let st = group_settings_from(&info);
        assert!(
            st.block_name
                && st.sign_admin_msg
                && !st.lock_create_post
                && st.join_appr
                && st.lock_view_member
        );
        assert_eq!(group_kind_from(&info), (None, None));
        assert!(!is_community(None, None));
        assert!(is_community(Some(GROUP_TYPE_COMMUNITY), Some(3)));
        assert!(is_community(Some(1), Some(101)));
        assert!(!is_community(Some(1), Some(100)));
        assert_eq!(ttl_caption(86_400_000), "1 ngày");
        assert_eq!(ttl_caption(1_000), "1000 ms");
        let (next, patch) = st.with_change(GroupSettingKey::LockSendMsg, true);
        assert!(next.lock_send_msg);
        assert_eq!(patch.lock_send_msg, Some(1));

        let pending = serde_json::json!({
            "data": {
                "users": [
                    { "uid": "11", "dpn": "Minh Anh" },
                    { "userId": 22, "displayName": "Thành" }
                ]
            }
        });
        let list = group_pending_from(&pending);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "Minh Anh");
        assert_eq!(list[1].uid, "22");

        let link = group_link_from(&serde_json::json!({
            "data": { "link": "https://zalo.me/g/abc", "enabled": 1 }
        }));
        assert!(link.enabled);
        assert_eq!(link.url, "https://zalo.me/g/abc");
    }
}
