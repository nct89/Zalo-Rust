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
    /// From `get_friends` (Contacts › Friends), not a group member or sender.
    pub friend: bool,
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
                        friend: v.get("friend").and_then(Value::as_bool).unwrap_or(false),
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
                    serde_json::json!({"name": p.name, "avatar": p.avatar, "members": p.members, "memberIds": p.member_ids, "friend": p.friend}),
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
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}

/// `~/.cache/zalo-rust/gpui-profiles.json`.
#[must_use]
pub fn cache_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let xdg = zalo_core::XdgDirs::from_env().ok()?;
    let dir = xdg.cache_home().join("zalo-rust");
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
            if let Some(avatar) = find_str(&v, &["avatar", "fullAvt", "avt"]) {
                out.map.insert(
                    ME.into(),
                    Profile {
                        name: find_str(&v, &["displayName", "zaloName"]).unwrap_or_default(),
                        avatar,
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
    if !profile.is_empty() && !is_id_label(thread_id, profile) {
        return profile.to_owned();
    }
    let current = current.trim();
    if !current.is_empty() {
        return current.to_owned();
    }
    thread_id.to_owned()
}

/// `get_multi_group_info` → `gridInfoMap` → `g_<grid>` entries.
#[must_use]
pub fn groups_from(value: &Value) -> HashMap<String, Profile> {
    let mut map = HashMap::new();
    let Some(obj) = value
        .get("data")
        .unwrap_or(value)
        .get("gridInfoMap")
        .and_then(Value::as_object)
    else {
        return map;
    };
    for (key, item) in obj {
        let raw = item.get("groupId").and_then(Value::as_str).unwrap_or(key);
        let clean = raw.strip_prefix("g_").unwrap_or(raw);
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
                avatar: find_str(item, &["fullAvt", "avt", "avatar"]).unwrap_or_default(),
                members,
                member_ids: member_ids(item),
                friend: false,
            },
        );
    }
    map
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
        if !avatar.starts_with("https://") && name.is_empty() {
            continue;
        }
        map.insert(
            uid,
            Profile {
                name,
                avatar,
                friend: false,
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
            .is_some_and(|p| p.avatar.starts_with("https://"));
        if !has && !out.contains(uid) {
            out.push(uid.clone());
        }
    }
    out
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
            if !avatar.starts_with("https://") {
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

/// First https / non-empty string under one of `keys`, searching nested objects.
pub(crate) fn find_str(v: &Value, keys: &[&str]) -> Option<String> {
    match v {
        Value::Object(map) => {
            for k in keys {
                if let Some(s) = map
                    .get(*k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    if !k.contains("av") || s.starts_with("https://") {
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
            "200000000000000002": {"name": "Team Group", "fullAvt": "https://ava-grp-talk.zadn.vn/x.jpg", "totalMember": 4, "memVerList": ["11_0", "22_3"]}
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
        assert_eq!(back.alias_or("1", "Original"), "User One");
        assert_eq!(back.saved_at, 5);
        assert!(back.stale(5 + MAX_AGE_MS + 1));
        assert!(!back.stale(6));
        let id = "g_200000000000000002";
        assert_eq!(group_display_name(id, id, "Team Group"), "Team Group");
        assert_eq!(group_display_name(id, id, ""), id);
        assert_eq!(group_display_name(id, "Old Name", id), "Old Name");
        assert_eq!(group_display_name(id, "Old Name", "New Name"), "New Name");
        assert!(is_id_label(id, "200000000000000002"));
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
