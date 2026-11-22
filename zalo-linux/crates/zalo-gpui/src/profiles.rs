//! Avatars, display names and group member counts.
//!
//! Zalo PC's Index.db cannot be joined (its user ids are scoped to the PC
//! app and differ from this client's), so profiles come from the same read
//! calls `zalo-gui` makes: `get_friends`, `get_group_list` →
//! `get_multi_group_info`, and `get_me`. The result is cached in
//! `~/.cache/zalo-rust/gpui-profiles.json` and refreshed at most once a day,
//! in the background. Avatar images reuse the CDN photo cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use zalo_client::{Device, StoredSession, ZaloApi};

/// Key of the signed-in account in the profile map.
pub const ME: &str = "me";
/// Refresh the cached profiles after this long.
const MAX_AGE_MS: i64 = 24 * 3_600_000;

/// One person or group.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    /// Display name from the server.
    pub name: String,
    /// https avatar URL, empty when none.
    pub avatar: String,
    /// Group member count (0 for people).
    pub members: u32,
    /// Group member uids (`memberIds` / `memVerList`), empty for people.
    pub member_ids: Vec<String>,
    /// `creatorId`. Empty until a group-info response has been stored.
    pub creator_id: String,
    /// `adminIds`, in server order.
    pub admin_ids: Vec<String>,
    /// From `get_friends` (Contacts › Friends), not a group member or sender.
    pub friend: bool,
    /// Cross-platform global noise ID (`globalId`) used by Sync 2 and PC→Phone Backup.
    pub global_id: String,
}

/// Cached map keyed by thread id (`<uid>` / `g_<grid>`) plus [`ME`].
#[derive(Clone, Debug, Default)]
pub struct Profiles {
    /// Entries.
    pub map: HashMap<String, Profile>,
    /// Friend aliases (`uid -> alias`) loaded from `/api/alias/list`.
    pub aliases: HashMap<String, String>,
    /// Unix ms of the last successful fetch.
    pub saved_at: i64,
}

impl Profiles {
    /// Friend alias when set, otherwise `fallback`.
    #[must_use]
    pub fn alias_or<'a>(&'a self, uid: &str, fallback: &'a str) -> &'a str {
        self.aliases
            .get(uid)
            .map(String::as_str)
            .filter(|a| !a.is_empty())
            .unwrap_or(fallback)
    }

    /// True when the cache is missing or older than a day.
    #[must_use]
    pub fn stale(&self, now_ms: i64) -> bool {
        self.map.is_empty() || now_ms - self.saved_at > MAX_AGE_MS
    }

    /// Read the disk cache (`None` path in tests).
    #[must_use]
    pub fn load(path: Option<&Path>) -> Self {
        let Some(value) = path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            return Self::default();
        };
        let mut map = HashMap::new();
        if let Some(obj) = value.get("map").and_then(Value::as_object) {
            for (k, v) in obj {
                map.insert(
                    k.clone(),
                    Profile {
                        name: str_of(v, "name"),
                        avatar: str_of(v, "avatar"),
                        members: v.get("members").and_then(Value::as_u64).unwrap_or(0) as u32,
                        member_ids: v
                            .get("memberIds")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        creator_id: str_of(v, "creatorId"),
                        admin_ids: v
                            .get("adminIds")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        friend: v.get("friend").and_then(Value::as_bool).unwrap_or(false),
                        global_id: str_of(v, "globalId"),
                    },
                );
            }
        }
        let mut aliases = HashMap::new();
        if let Some(obj) = value.get("aliases").and_then(Value::as_object) {
            for (k, v) in obj {
                if let Some(alias) = v.as_str().filter(|s| !s.is_empty()) {
                    aliases.insert(k.clone(), alias.to_owned());
                }
            }
        }
        Self {
            map,
            aliases,
            saved_at: value.get("savedAt").and_then(Value::as_i64).unwrap_or(0),
        }
    }

    /// Write the disk cache.
    pub fn save(&self, path: &Path) {
        let map: serde_json::Map<String, Value> = self
            .map
            .iter()
            .map(|(k, p)| {
                (
                    k.clone(),
                    serde_json::json!({"name": p.name, "avatar": p.avatar, "members": p.members, "memberIds": p.member_ids, "creatorId": p.creator_id, "adminIds": p.admin_ids, "friend": p.friend, "globalId": p.global_id}),
                )
            })
            .collect();
        let aliases: serde_json::Map<String, Value> = self
            .aliases
            .iter()
            .filter(|(_, a)| !a.is_empty())
            .map(|(k, a)| (k.clone(), Value::String(a.clone())))
            .collect();
        let text = serde_json::json!({"savedAt": self.saved_at, "map": map, "aliases": aliases})
            .to_string();
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Merge a fresh server fetch without wiping out previously cached friends
    /// (e.g. when `getfriends` is rate-limited 429), `getgi/v2`-only groups
    /// omitted by `getlg/v4`, or `getprofiles/v2` member avatars.
    pub fn merge(&mut self, fresh: Self) {
        if fresh.saved_at > self.saved_at {
            self.saved_at = fresh.saved_at;
        }
        for (uid, alias) in fresh.aliases {
            if !alias.is_empty() {
                self.aliases.insert(uid, alias);
            }
        }
        for (key, got) in fresh.map {
            let slot = self.map.entry(key).or_default();
            if !got.name.is_empty() {
                slot.name = got.name;
            }
            if !got.avatar.is_empty()
                && (crate::sidebar::is_real_avatar(&got.avatar)
                    || !crate::sidebar::is_real_avatar(&slot.avatar))
            {
                slot.avatar = got.avatar;
            }
            if got.members > 0 {
                slot.members = got.members;
            }
            if !got.member_ids.is_empty() {
                slot.member_ids = got.member_ids;
            }
            if !got.creator_id.is_empty() {
                slot.creator_id = got.creator_id;
            }
            if !got.admin_ids.is_empty() {
                slot.admin_ids = got.admin_ids;
            }
            if got.friend {
                slot.friend = true;
            }
            if !got.global_id.is_empty() {
                slot.global_id = got.global_id;
            }
        }
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}

/// String or integer id (`creatorId` arrives as either).
fn id_of(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| n.as_i64().and_then(|i| u64::try_from(i).ok()))
            .map(|n| n.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn id_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(text) if !text.is_empty() => Some(text.clone()),
                    Value::Number(n) => n.as_u64().map(|n| n.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Profile cache for the account that is open.
///
/// The file sits in that account's directory. The old global
/// `~/.cache/zalo-rust/gpui-profiles.json` mixed names and avatars of every
/// account that had logged in on this machine, so it is not read once an
/// account directory exists.
#[must_use]
pub fn cache_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    if let Some(dir) = crate::data::active_data_dir() {
        return Some(dir.join("profiles.json"));
    }
    let paths = zalo_core::AppPaths::from_env("zalo-rust").ok()?;
    let dir = paths.cache_dir().to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("gpui-profiles.json"))
}

/// Friends + groups + me. Blocking: background executor only.
///
/// # Errors
///
/// Returns a short message without a usable session or when every call fails.
pub fn fetch(dir: &Path, now_ms: i64) -> Result<Profiles, String> {
    let session = StoredSession::load(dir).map_err(|e| e.to_string())?;
    if session.zpw_sek().is_none() || !dir.join("device.json").is_file() {
        return Err("no session".into());
    }
    let device = Device::load_or_create(dir).map_err(|e| e.to_string())?;
    let mut api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let mut out = Profiles {
        map: HashMap::new(),
        aliases: HashMap::new(),
        saved_at: now_ms,
    };
    let mut errors: Vec<String> = Vec::new();
    rt.block_on(async {
        // Login info carries the friend / group / profile hosts (zalo-gui order).
        if let Err(e) = api.get_login_info().await {
            errors.push(format!("login_info: {e}"));
        }
        match api.get_friends().await {
            Ok(v) => out.map.extend(friends_from(&v)),
            Err(e) => errors.push(format!("friends: {e}")),
        }
        match api.get_group_list().await {
            Ok(list) => {
                let ids = group_ids(&list);
                for chunk in ids.chunks(50) {
                    match api.get_multi_group_info(chunk).await {
                        Ok(v) => {
                            out.map.extend(groups_from(&v));
                            for (uid, profile) in member_profiles_from(&v) {
                                out.map.entry(uid).or_insert(profile);
                            }
                        }
                        Err(e) => errors.push(format!("groups: {e}")),
                    }
                }
            }
            Err(e) => errors.push(format!("group_list: {e}")),
        }
        match api.get_aliases(1, 200).await {
            Ok(v) => out.aliases = aliases_from(&v),
            Err(e) => errors.push(format!("aliases: {e}")),
        }
        let me = api.get_me().await;
        if let Err(e) = &me {
            errors.push(format!("me: {e}"));
        }
        if let Ok(v) = me {
            let avatar = find_str(&v, &["avatar", "fullAvt", "avt"]).unwrap_or_default();
            let name = find_str(&v, &["displayName", "zaloName"]).unwrap_or_default();
            let global_id = find_str(&v, &["globalId"]).unwrap_or_default();
            if !avatar.is_empty() || !name.is_empty() || !global_id.is_empty() {
                out.map.insert(
                    ME.into(),
                    Profile {
                        name,
                        avatar,
                        global_id,
                        ..Profile::default()
                    },
                );
            }
        }
    });
    if out.map.is_empty() {
        return Err(format!(
            "profile calls returned nothing: {}",
            errors.join("; ")
        ));
    }
    if !errors.is_empty() {
        eprintln!("zalo-gpui profiles (partial): {}", errors.join("; "));
    }
    Ok(out)
}

/// `get_friends` → `data[]` with `userId`, `displayName`, `avatar`.
#[must_use]
pub fn friends_from(value: &Value) -> HashMap<String, Profile> {
    let mut map = HashMap::new();
    let list = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.as_array());
    for item in list.into_iter().flatten() {
        let Some(uid) = item.get("userId").and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        }) else {
            continue;
        };
        map.insert(
            uid,
            Profile {
                name: find_str(item, &["displayName", "zaloName"]).unwrap_or_default(),
                avatar: find_str(item, &["avatar"]).unwrap_or_default(),
                friend: true,
                global_id: find_str(item, &["globalId"]).unwrap_or_default(),
                ..Profile::default()
            },
        );
    }
    map
}

/// `get_group_list` → `gridVerMap` keys.
#[must_use]
pub fn group_ids(value: &Value) -> Vec<String> {
    value
        .get("data")
        .unwrap_or(value)
        .get("gridVerMap")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// True when `name` is empty of meaning: the raw group id, with or without `g_`.
#[must_use]
pub fn is_id_label(thread_id: &str, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return true;
    }
    let bare = thread_id.trim().trim_start_matches("g_");
    let prefixed = format!("g_{bare}");
    name == thread_id.trim() || name == bare || name == prefixed
}

/// Name to paint for a group. A real server label replaces the row. An empty
/// label or the raw id leaves `current` alone, so the list does not fall back
/// to `g_<id>` over a name that is already shown.
#[must_use]
pub fn group_display_name(thread_id: &str, current: &str, profile_name: &str) -> String {
    let profile = profile_name.trim();
    let current_trim = current.trim();
    // Error 164 has no group name. "Nhóm đã rời" is only the placeholder.
    // A title the list already shows (from the phone store) stays.
    if profile == "Nhóm đã rời" {
        if !current_trim.is_empty()
            && current_trim != "Nhóm"
            && !is_id_label(thread_id, current_trim)
        {
            return current_trim.to_owned();
        }
        return "Nhóm đã rời".to_owned();
    }
    if !profile.is_empty() && !is_id_label(thread_id, profile) {
        return profile.to_owned();
    }
    if !current_trim.is_empty() {
        return current_trim.to_owned();
    }
    thread_id.to_owned()
}

/// `get_multi_group_info` / `get_group_info_v2` → `gridInfoMap` (or direct `data`) → `g_<grid>` entries.
#[must_use]
pub fn groups_from(value: &Value) -> HashMap<String, Profile> {
    let mut map = HashMap::new();
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    if let Some(obj) = root.get("gridInfoMap").and_then(Value::as_object) {
        for (key, item) in obj {
            insert_group_entry(&mut map, key, item);
        }
        return map;
    }
    if let Some(gid) = root
        .get("groupId")
        .or_else(|| root.get("grid"))
        .and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .filter(|s| !s.is_empty())
    {
        insert_group_entry(&mut map, &gid, root);
    }
    map
}

fn insert_group_entry(map: &mut HashMap<String, Profile>, key: &str, item: &Value) {
    let raw = item
        .get("groupId")
        .and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| key.to_owned());
    let clean = raw.strip_prefix("g_").unwrap_or(&raw);
    if clean.is_empty() {
        return;
    }
    let members = item
        .get("totalMember")
        .and_then(Value::as_u64)
        .or_else(|| {
            item.get("memberIds")
                .and_then(Value::as_array)
                .map(|a| a.len() as u64)
        })
        .unwrap_or(0) as u32;
    map.insert(
        format!("g_{clean}"),
        Profile {
            name: find_str(item, &["name", "displayName"]).unwrap_or_default(),
            avatar: best_avatar_str(item, &["fullAvt", "avt", "avatar"]).unwrap_or_default(),
            members,
            member_ids: member_ids(item),
            creator_id: id_of(item, "creatorId"),
            admin_ids: id_list(item, "adminIds"),
            friend: false,
            global_id: find_str(item, &["globalId"]).unwrap_or_default(),
        },
    );
}

/// Prefer a real photo URL (non-`/default`) across `keys`; fall back to any
/// `http(s)://` URL so `/default` placeholders never shadow a real avatar field.
fn best_avatar_str(v: &Value, keys: &[&str]) -> Option<String> {
    let mut fallback = None;
    if let Value::Object(map) = v {
        for k in keys {
            if let Some(s) = map
                .get(*k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
            {
                if crate::sidebar::is_real_avatar(s) {
                    return Some(s.to_owned());
                }
                if fallback.is_none() {
                    fallback = Some(s.to_owned());
                }
            }
        }
    }
    fallback.or_else(|| find_str(v, keys))
}

/// `getprofiles/v2` → `data.changed_profiles` keyed by uid.
///
/// Entries without an https avatar are kept when they carry a display name,
/// so a later paint can still label the person.
#[must_use]
pub fn profiles_from_v2(value: &Value) -> HashMap<String, Profile> {
    let mut map = HashMap::new();
    let Some(changed) = value
        .get("data")
        .unwrap_or(value)
        .get("changed_profiles")
        .and_then(Value::as_object)
    else {
        return map;
    };
    for (key, item) in changed {
        if !item.is_object() {
            continue;
        }
        let uid = find_str(item, &["userId", "uid", "id"]).unwrap_or_else(|| key.clone());
        if uid.is_empty() || uid.starts_with("g_") {
            continue;
        }
        let avatar = find_str(item, &["avatar", "fullAvt", "avt"]).unwrap_or_default();
        let name = find_str(item, &["displayName", "zaloName", "dName"]).unwrap_or_default();
        if !avatar.starts_with("https://") && !avatar.starts_with("http://") && name.is_empty() {
            continue;
        }
        map.insert(
            uid,
            Profile {
                name,
                avatar,
                friend: false,
                global_id: find_str(item, &["globalId"]).unwrap_or_default(),
                ..Profile::default()
            },
        );
    }
    map
}

/// True for a numeric Zalo user id. `getprofiles/v2` answers 114 for a chunk
/// that contains a 32-character zid.
#[must_use]
pub fn is_numeric_uid(uid: &str) -> bool {
    !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit())
}

/// Group thread ids that still lack a real server name, or lack both a custom
/// group photo and member ids (needed to paint the member collage).
#[must_use]
pub fn groups_needing_info(profiles: &Profiles, thread_ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for id in thread_ids {
        if !id.starts_with("g_") || id.starts_with("preview:") {
            continue;
        }
        let needs = match profiles.map.get(id) {
            None => true,
            Some(profile) => {
                let name = profile.name.trim();
                if name == "Nhóm đã rời" {
                    false
                } else {
                    name.is_empty()
                        || name == "Nhóm"
                        || is_id_label(id, name)
                        || (!crate::sidebar::is_real_avatar(&profile.avatar)
                            && profile.member_ids.is_empty())
                }
            }
        };
        if needs && !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

/// Uids in `want` that have no https avatar and can be asked on `getprofiles/v2`.
#[must_use]
pub fn missing_avatar_uids(profiles: &Profiles, want: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for uid in want {
        if !is_numeric_uid(uid) || uid == ME || uid.starts_with("g_") || uid.starts_with("preview:")
        {
            continue;
        }
        let has = profiles
            .map
            .get(uid)
            .is_some_and(|p| p.avatar.starts_with("https://") || p.avatar.starts_with("http://"));
        if !has && !out.contains(uid) {
            out.push(uid.clone());
        }
    }
    out
}

/// Keys only. Group replies can carry encrypted blobs; do not print those.
fn value_shape(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let keys: Vec<&str> = map.keys().take(8).map(String::as_str).collect();
            let nested = map
                .get("data")
                .filter(|v| !v.is_string())
                .map(|v| format!(" data={}", value_shape(v)))
                .unwrap_or_default();
            format!("{{{keys:?}}}{nested}")
        }
        Value::Array(items) => format!("[{}]", items.len()),
        Value::String(text) => format!("str{}", text.len()),
        Value::Null => "null".into(),
        Value::Number(n) => n.to_string(),
        Value::Bool(on) => on.to_string(),
    }
}

/// Group name, photo and members for thread ids missing from the friend/group cache.
///
/// `getlg/v4` does not list every group that `fetchImportant` already showed.
/// Those rows stay on the placeholder "Nhóm" until `getmg-v2` is asked for
/// their ids. The second map is member photos, inserted only when that uid
/// is not already a friend.
///
/// # Errors
///
/// Returns a short message without a session or when every chunk fails.
pub fn fetch_missing_groups(
    dir: &Path,
    ids: &[String],
) -> Result<(HashMap<String, Profile>, HashMap<String, Profile>), String> {
    if ids.is_empty() {
        return Ok((HashMap::new(), HashMap::new()));
    }
    eprintln!("zalo-gpui group gaps: hỏi getmg-v2 cho {} nhóm", ids.len());
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let session = StoredSession::load(dir).map_err(|e| e.to_string())?;
    if session.zpw_sek().is_none() || !dir.join("device.json").is_file() {
        return Err("no session".into());
    }
    let device = Device::load_or_create(dir).map_err(|e| e.to_string())?;
    let mut api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let mut groups = HashMap::new();
    let mut members = HashMap::new();
    let mut errors = Vec::new();
    rt.block_on(async {
        if let Err(e) = api.get_login_info().await {
            errors.push(format!("login_info: {e}"));
        }
        for chunk in ids.chunks(50) {
            let owned: Vec<String> = chunk.iter().map(|id| (*id).clone()).collect();
            match api.get_multi_group_info(&owned).await {
                Ok(value) => {
                    let parsed = groups_from(&value);
                    if parsed.is_empty() {
                        eprintln!(
                            "zalo-gpui group gaps: getmg-v2 không có nhóm ({})",
                            value_shape(&value)
                        );
                    }
                    groups.extend(parsed);
                    members.extend(member_profiles_from(&value));
                }
                Err(e) => errors.push(format!("getmg: {e}")),
            }
            // `getlg/v4` omits some threads that are already on screen.
            // `getmg-v2` then answers an empty map. `getgi/v2` is the
            // per-group profile the info column already uses.
            for id in &owned {
                let key = if id.starts_with("g_") {
                    id.clone()
                } else {
                    format!("g_{id}")
                };
                if groups.contains_key(&key) {
                    continue;
                }
                match api.get_group_info_v2(id, 1).await {
                    Ok(value) => {
                        let parsed = groups_from(&value);
                        if parsed.is_empty() {
                            eprintln!(
                                "zalo-gpui group gaps: getgi {key} không có tên ({})",
                                value_shape(&value)
                            );
                        }
                        // Keep the thread id even when the server keys the
                        // map by a different grid.
                        if parsed.len() == 1 && !parsed.contains_key(&key) {
                            if let Some(profile) = parsed.into_values().next() {
                                groups.insert(key, profile);
                            }
                        } else {
                            groups.extend(parsed);
                        }
                        members.extend(member_profiles_from(&value));
                        for member in zalo_client::GroupMember::list_from(&value) {
                            let Some(avatar) = member.avatar else { continue };
                            members.entry(member.uid).or_insert(Profile {
                                name: member.name,
                                avatar,
                                friend: false,
                                ..Profile::default()
                            });
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        // 164: the account is no longer in the group. The
                        // server will not send a name or a photo.
                        if msg.contains("164") {
                            groups.insert(
                                key,
                                Profile {
                                    name: "Nhóm đã rời".into(),
                                    ..Profile::default()
                                },
                            );
                        } else {
                            errors.push(format!("getgi {key}: {msg}"));
                        }
                    }
                }
            }
        }
    });
    eprintln!(
        "zalo-gpui group gaps: nhận {} nhóm, {} thành viên{}",
        groups.len(),
        members.len(),
        if errors.is_empty() {
            String::new()
        } else {
            format!(", lỗi {}", errors.join("; "))
        }
    );
    let _ = std::io::Write::flush(&mut std::io::stderr());
    if groups.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok((groups, members))
}

/// Fetches avatar URLs for `uids` via `/api/social/friend/getprofiles/v2`.
///
/// # Errors
///
/// Returns a short message without a session or when every chunk fails.
pub fn fetch_missing(dir: &Path, uids: &[String]) -> Result<HashMap<String, Profile>, String> {
    if uids.is_empty() {
        return Ok(HashMap::new());
    }
    let session = StoredSession::load(dir).map_err(|e| e.to_string())?;
    if session.zpw_sek().is_none() || !dir.join("device.json").is_file() {
        return Err("no session".into());
    }
    let device = Device::load_or_create(dir).map_err(|e| e.to_string())?;
    let mut api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let mut out = HashMap::new();
    let mut errors = Vec::new();
    rt.block_on(async {
        if let Err(e) = api.get_login_info().await {
            errors.push(format!("login_info: {e}"));
        }
        let numeric: Vec<&String> = uids.iter().filter(|uid| is_numeric_uid(uid)).collect();
        for chunk in numeric.chunks(20) {
            let refs: Vec<&str> = chunk.iter().map(|uid| uid.as_str()).collect();
            match api.get_profiles_v2(&refs).await {
                Ok(value) => out.extend(profiles_from_v2(&value)),
                Err(e) => errors.push(e.to_string()),
            }
        }
    });
    if out.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(out)
}

/// Member photos from `currentMems`. Callers insert these only when the uid
/// is not already a friend, so a friend name is not replaced by `dName`.
#[must_use]
pub fn member_profiles_from(value: &Value) -> HashMap<String, Profile> {
    let mut map = HashMap::new();
    let Some(obj) = value
        .get("data")
        .unwrap_or(value)
        .get("gridInfoMap")
        .and_then(Value::as_object)
    else {
        return map;
    };
    for item in obj.values() {
        let Some(mems) = item.get("currentMems").and_then(Value::as_array) else {
            continue;
        };
        for mem in mems {
            let Some(uid) = mem.get("id").or_else(|| mem.get("userId")).and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            }) else {
                continue;
            };
            let avatar = find_str(mem, &["avatar", "avt", "fullAvt"]).unwrap_or_default();
            if !avatar.starts_with("https://") && !avatar.starts_with("http://") {
                continue;
            }
            map.entry(uid).or_insert(Profile {
                name: find_str(mem, &["dName", "displayName", "zaloName"]).unwrap_or_default(),
                avatar,
                friend: false,
                ..Profile::default()
            });
        }
    }
    map
}

/// `memberIds: ["uid", …]`, else `memVerList: ["uid_ver", …]`.
fn member_ids(item: &Value) -> Vec<String> {
    let list = |key: &str| -> Vec<String> {
        item.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .or_else(|| v.as_i64().map(|n| n.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let ids = list("memberIds");
    if !ids.is_empty() {
        return ids;
    }
    list("memVerList")
        .into_iter()
        .map(|s| s.split('_').next().unwrap_or_default().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `alias/list` → `uid -> alias`.
#[must_use]
pub fn aliases_from(value: &Value) -> HashMap<String, String> {
    let items = value
        .get("data")
        .and_then(|d| d.get("items").or(Some(d)))
        .and_then(Value::as_array)
        .or_else(|| value.get("items").and_then(Value::as_array));
    let Some(list) = items else {
        return HashMap::new();
    };
    list.iter()
        .filter_map(|item| {
            let uid = ["userId", "friendId", "uid"]
                .iter()
                .find_map(|k| item.get(*k).and_then(Value::as_str))?;
            let alias = item
                .get("alias")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            (!uid.is_empty() && !alias.is_empty()).then(|| (uid.to_owned(), alias.to_owned()))
        })
        .collect()
}

/// First http(s) / non-empty string under one of `keys`, searching nested objects.
pub(crate) fn find_str(v: &Value, keys: &[&str]) -> Option<String> {
    match v {
        Value::Object(map) => {
            for k in keys {
                if let Some(s) = map
                    .get(*k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    let is_avatar_key = k.eq_ignore_ascii_case("fullAvt")
                        || k.eq_ignore_ascii_case("avt")
                        || k.eq_ignore_ascii_case("avatar");
                    if !is_avatar_key || s.starts_with("https://") || s.starts_with("http://") {
                        return Some(s.to_owned());
                    }
                }
            }
            map.values().find_map(|c| find_str(c, keys))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_friends_groups_and_cache() {
        let friends = serde_json::json!({"data": [
            {"userId": "100000000000000001", "displayName": "Alice", "avatar": "https://s120-ava-talk.zadn.vn/a.jpg"},
            {"userId": "1", "displayName": "No avatar", "avatar": ""}
        ]});
        let f = friends_from(&friends);
        assert_eq!(
            f["100000000000000001"].avatar,
            "https://s120-ava-talk.zadn.vn/a.jpg"
        );
        assert_eq!(f["1"].avatar, "");
        assert!(f["1"].friend);
        let groups = serde_json::json!({"data": {"gridInfoMap": {
            "200000000000000002": {"name": "Team Group", "fullAvt": "https://ava-grp-talk.zadn.vn/x.jpg", "totalMember": 4, "memVerList": ["11_0", "22_3"], "creatorId": "11", "adminIds": ["22"]}
        }}});
        let g = groups_from(&groups);
        assert_eq!(g["g_200000000000000002"].members, 4);
        assert_eq!(
            group_ids(&serde_json::json!({"data": {"gridVerMap": {"9": "1"}}})),
            ["9"]
        );
        let members = member_profiles_from(&serde_json::json!({"data": {"gridInfoMap": {
            "9": {"currentMems": [
                {"id": "3", "dName": "C", "avatar": "https://x/c.jpg"},
                {"id": "1", "dName": "A"}
            ]}
        }}}));
        assert_eq!(members["3"].avatar, "https://x/c.jpg");
        assert!(!members.contains_key("1"));
        let mut aliases = aliases_from(&serde_json::json!({
            "data": { "items": [{"userId": "1", "alias": "User One"}] }
        }));
        let dir = std::env::temp_dir().join("zalo-gpui-profiles-test.json");
        let p = Profiles {
            map: g,
            aliases: std::mem::take(&mut aliases),
            saved_at: 5,
        };
        assert_eq!(p.alias_or("1", "Original"), "User One");
        assert_eq!(p.alias_or("2", "Original"), "Original");
        p.save(&dir);
        let back = Profiles::load(Some(&dir));
        assert_eq!(back.map["g_200000000000000002"].name, "Team Group");
        assert_eq!(back.map["g_200000000000000002"].member_ids, ["11", "22"]);
        assert_eq!(back.map["g_200000000000000002"].creator_id, "11");
        assert_eq!(back.map["g_200000000000000002"].admin_ids, ["22"]);
        assert_eq!(back.alias_or("1", "Original"), "User One");
        assert_eq!(back.saved_at, 5);
        assert!(back.stale(5 + MAX_AGE_MS + 1));
        assert!(!back.stale(6));
        let id = "g_200000000000000002";
        assert_eq!(group_display_name(id, id, "Team Group"), "Team Group");
        assert_eq!(group_display_name(id, id, ""), id);
        assert_eq!(group_display_name(id, "Old Name", id), "Old Name");
        assert_eq!(group_display_name(id, "Old Name", "New Name"), "New Name");
        assert_eq!(
            group_display_name(id, "Phòng Zoom", "Nhóm đã rời"),
            "Phòng Zoom"
        );
        assert_eq!(group_display_name(id, id, "Nhóm đã rời"), "Nhóm đã rời");
        assert_eq!(group_display_name(id, "Nhóm", "Nhóm đã rời"), "Nhóm đã rời");
        assert!(is_id_label(id, "200000000000000002"));
        let mut cached = Profiles::default();
        cached.map.insert(
            id.into(),
            Profile {
                name: "Team Group".into(),
                member_ids: vec!["11".into()],
                ..Profile::default()
            },
        );
        cached.map.insert(
            "g_888".into(),
            Profile {
                name: "Missing Visual".into(),
                ..Profile::default()
            },
        );
        let want = vec![
            id.to_owned(),
            "g_888".into(),
            "g_999".into(),
            "g_999".into(),
            "42".into(),
        ];
        assert_eq!(
            groups_needing_info(&cached, &want),
            vec!["g_888".to_owned(), "g_999".to_owned()]
        );

        let fallback_avt = serde_json::json!({"data": {"gridInfoMap": {
            "77": {"name": "Fallback", "fullAvt": "default", "avt": "https://ava-grp-talk.zadn.vn/real.jpg", "totalMember": 3}
        }}});
        assert_eq!(
            groups_from(&fallback_avt)["g_77"].avatar,
            "https://ava-grp-talk.zadn.vn/real.jpg"
        );

        let mut merged = cached.clone();
        merged.map.insert(
            "555".into(),
            Profile {
                name: "Friend Kept".into(),
                avatar: "https://ava.example/555.jpg".into(),
                friend: true,
                ..Profile::default()
            },
        );
        let mut partial = Profiles {
            map: HashMap::new(),
            aliases: HashMap::new(),
            saved_at: 99,
        };
        partial.map.insert(
            "g_77".into(),
            Profile {
                name: "Fallback".into(),
                avatar: "https://ava-grp-talk.zadn.vn/real.jpg".into(),
                ..Profile::default()
            },
        );
        merged.merge(partial);
        assert!(merged.map.contains_key("555"));
        assert_eq!(merged.map["g_77"].name, "Fallback");
        assert_eq!(merged.saved_at, 99);
    }

    #[test]
    fn profiles_v2_fills_only_missing_avatars() {
        let value = serde_json::json!({"data": {"changed_profiles": {
            "9": {"userId": "9", "displayName": "Lan", "avatar": "https://ava.example/9.jpg"},
            "8": {"userId": "8", "displayName": "No photo", "avatar": ""},
            "g_1": {"userId": "g_1", "displayName": "Group"}
        }}});
        let got = profiles_from_v2(&value);
        assert_eq!(got["9"].avatar, "https://ava.example/9.jpg");
        assert_eq!(got["9"].name, "Lan");
        assert_eq!(got["8"].name, "No photo");
        assert!(got["8"].avatar.is_empty());
        assert!(!got.contains_key("g_1"));
        let mut profiles = Profiles::default();
        profiles.map.insert(
            "9".into(),
            Profile {
                avatar: "https://ava.example/already.jpg".into(),
                ..Profile::default()
            },
        );
        let zid = "MVHFH975IGFTVPPHN3NRUNPFJDPA3C80".to_owned();
        let missing = missing_avatar_uids(
            &profiles,
            &["9".into(), "8".into(), ME.into(), "g_2".into(), zid],
        );
        assert_eq!(missing, vec!["8".to_owned()]);
        assert!(is_numeric_uid("8"));
        assert!(!is_numeric_uid("MVHFH975IGFTVPPHN3NRUNPFJDPA3C80"));
    }
}
