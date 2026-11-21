//! PC ↔ Global Noise ID resolver for Sync 2 (denoise) and PC → Phone Backup (`GET_BACKUP_NOISED_IDS`),
//! matching Electron `GetNoisedIdsHandler` (`main-startup`), `compact1395.js`, `compact2388.js`,
//! and `compact4429.js` (2 retries, 300ms delay).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::Value;
use zalo_client::ZaloApi;

use crate::profiles::Profiles;

const BATCH_SIZE: usize = 50;
const RETRY_COUNT: usize = 2;
const RETRY_DELAY_MS: u64 = 300;

/// Resolves PC IDs (`"<uid>"` for 1-1, `"g_<grid>"` for groups) to cross-platform global noise IDs.
///
/// 1. Uses cached `Profiles` (`profile.global_id`) first.
/// 2. Falls back to `GET /api/gid/encrypt` for uncached 1-1 IDs (2 retries, 300ms delay).
/// 3. Falls back to `get_multi_group_info` for uncached group IDs.
pub async fn resolve_pc_to_global(
    api: &ZaloApi,
    profiles: &Profiles,
    pc_ids: &[String],
) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut missing_users: Vec<String> = Vec::new();
    let mut missing_groups: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for raw in pc_ids {
        let id = raw.trim();
        if id.is_empty() || id == "0" || id == "-1" || !seen.insert(id) {
            continue;
        }
        if let Some(p) = profiles.map.get(id) {
            if !p.global_id.is_empty() {
                out.insert(id.to_owned(), p.global_id.clone());
                continue;
            }
        }
        if let Some(grid) = id.strip_prefix("g_") {
            if !grid.is_empty() {
                missing_groups.push(grid.to_owned());
            }
        } else {
            missing_users.push(id.to_owned());
        }
    }

    // 1-1 users via `/api/gid/encrypt` (2 retries, 300ms delay).
    for chunk in missing_users.chunks(BATCH_SIZE) {
        let mut fetched = None;
        for attempt in 0..=RETRY_COUNT {
            match api.get_global_noise_ids(chunk).await {
                Ok(val) => {
                    fetched = Some(val);
                    break;
                }
                Err(_) if attempt < RETRY_COUNT => {
                    tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                }
                Err(_) => break,
            }
        }
        if let Some(val) = fetched {
            for (k, v) in parse_gid_map(&val) {
                out.insert(k, v);
            }
        }
    }

    // Groups via `get_multi_group_info` (`gridInfoMap[grid].globalId`).
    for chunk in missing_groups.chunks(BATCH_SIZE) {
        if let Ok(val) = api.get_multi_group_info(chunk).await {
            let root = val.get("data").unwrap_or(&val);
            if let Some(grid_map) = root.get("gridInfoMap").and_then(Value::as_object) {
                for (grid, info) in grid_map {
                    if let Some(gid) = info
                        .get("globalId")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        let clean = grid.strip_prefix("g_").unwrap_or(grid);
                        out.insert(format!("g_{clean}"), gid.to_owned());
                    }
                }
            }
        }
    }

    out
}

/// Resolves cross-platform global noise IDs back to PC IDs (`"<uid>"` for 1-1, `"g_<grid>"` for groups).
///
/// 1. Uses reverse lookup in `Profiles` (`profile.global_id -> pc_id`).
/// 2. Falls back to `GET /api/gid/decrypt` for uncached 1-1 global IDs (2 retries, 300ms delay).
pub async fn resolve_global_to_pc(
    api: &ZaloApi,
    profiles: &Profiles,
    global_ids: &[(String, bool)], // (global_id, is_group)
) -> HashMap<String, String> {
    let mut rev_users: HashMap<&str, &str> = HashMap::new();
    let mut rev_groups: HashMap<&str, &str> = HashMap::new();
    for (pc_id, prof) in &profiles.map {
        if prof.global_id.is_empty() {
            continue;
        }
        if pc_id.starts_with("g_") {
            rev_groups.insert(prof.global_id.as_str(), pc_id.as_str());
        } else if pc_id != crate::profiles::ME {
            rev_users.insert(prof.global_id.as_str(), pc_id.as_str());
        }
    }

    let mut out: HashMap<String, String> = HashMap::new();
    let mut missing_users: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for (gid, is_group) in global_ids {
        let g = gid.trim();
        if g.is_empty() || !seen.insert(g) {
            continue;
        }
        if *is_group {
            if let Some(&pc) = rev_groups.get(g) {
                out.insert(g.to_owned(), pc.to_owned());
            }
        } else if let Some(&pc) = rev_users.get(g) {
            out.insert(g.to_owned(), pc.to_owned());
        } else {
            missing_users.push(g.to_owned());
        }
    }

    for chunk in missing_users.chunks(BATCH_SIZE) {
        let mut fetched = None;
        for attempt in 0..=RETRY_COUNT {
            match api.get_uids_from_global_noise_ids(chunk).await {
                Ok(val) => {
                    fetched = Some(val);
                    break;
                }
                Err(_) if attempt < RETRY_COUNT => {
                    tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                }
                Err(_) => break,
            }
        }
        if let Some(val) = fetched {
            for (k, v) in parse_gid_map(&val) {
                out.insert(k, v);
            }
        }
    }

    out
}

/// Parses both `{ "data": { "k": "v" } }`, `{ "data": { "globalIds": { ... } } }`,
/// `{ "data": { "uids": { ... } } }`, and flat `{ "k": "v" }` response shapes from
/// `/api/gid/encrypt` and `/api/gid/decrypt`.
#[must_use]
pub fn parse_gid_map(value: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let candidate = root
        .get("globalIds")
        .or_else(|| root.get("uids"))
        .or_else(|| root.get("map"))
        .filter(|v| v.is_object())
        .unwrap_or(root);
    if let Some(obj) = candidate.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                out.insert(k.clone(), s.to_owned());
            } else if let Some(n) = v.as_i64() {
                out.insert(k.clone(), n.to_string());
            } else if let Some(n) = v.as_u64() {
                out.insert(k.clone(), n.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_flat_and_nested_gid_maps() {
        let flat = json!({ "data": { "123": "g_abc", "456": "g_def" } });
        let map = parse_gid_map(&flat);
        assert_eq!(map.get("123").map(String::as_str), Some("g_abc"));
        assert_eq!(map.get("456").map(String::as_str), Some("g_def"));

        let nested = json!({ "data": { "uids": { "g_abc": 123 } } });
        let map2 = parse_gid_map(&nested);
        assert_eq!(map2.get("g_abc").map(String::as_str), Some("123"));
    }
}
