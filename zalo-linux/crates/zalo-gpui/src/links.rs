//! Link previews for chat link cards.
//!
//! Zalo PC builds its cards from `/api/message/parselink` (thumb, title,
//! desc, src). Realtime `chat.recommended` messages already carry title /
//! description / thumb; older rows in `store.db` only kept the URL, so the
//! card asks `parselink` once per URL. Results live in
//! `~/.cache/zalo-rust/gpui-links.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// One card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkPreview {
    /// Page title.
    pub title: String,
    /// Description (one or two lines on the card).
    pub desc: String,
    /// https thumbnail, empty when none.
    pub thumb: String,
    /// Site label (`src`, else the host).
    pub src: String,
}

impl LinkPreview {
    /// True when there is nothing to draw beyond the URL.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.desc.is_empty() && self.thumb.is_empty()
    }
}

/// `~/.cache/zalo-rust/gpui-links.json`.
#[must_use]
pub fn cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("zalo-rust");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("gpui-links.json"))
}

/// Read the cache (empty in tests / when missing).
#[must_use]
pub fn load(path: Option<&Path>) -> HashMap<String, LinkPreview> {
    let Some(value) = path
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return HashMap::new();
    };
    value
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| {
                    let s = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
                    (
                        k.clone(),
                        LinkPreview {
                            title: s("title"),
                            desc: s("desc"),
                            thumb: s("thumb"),
                            src: s("src"),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Write the cache.
pub fn save(path: &Path, map: &HashMap<String, LinkPreview>) {
    let obj: serde_json::Map<String, Value> = map
        .iter()
        .map(|(k, p)| {
            (k.clone(), serde_json::json!({"title": p.title, "desc": p.desc, "thumb": p.thumb, "src": p.src}))
        })
        .collect();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, Value::Object(obj).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Decrypted `parselink` reply → preview (`data` may be nested twice).
#[must_use]
pub fn from_parselink(value: &Value) -> LinkPreview {
    // Seen live: `{error_code, data: {data: {thumb, title, desc, src, href}}}`.
    let mut root = value;
    while let Some(inner) = root.get("data").filter(|v| v.is_object()) {
        root = inner;
    }
    let s = |key: &str| {
        root.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let thumb = s("thumb");
    LinkPreview {
        title: s("title"),
        desc: s("desc"),
        thumb: if thumb.starts_with("https://") {
            thumb
        } else {
            String::new()
        },
        src: s("src"),
    }
}

/// `m:ss` for a duration in milliseconds.
#[must_use]
pub fn clock(ms: u64) -> String {
    let secs = ms.div_ceil(1000);
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parselink_and_cache_round_trip() {
        let v = serde_json::json!({"error_code": 0, "data": {"data": {"title": "Electron vs GPUI", "desc": "Discord", "thumb": "https://x/t.jpg", "src": "dev.to"}}});
        let p = from_parselink(&v);
        assert_eq!(p.title, "Electron vs GPUI");
        assert_eq!(p.src, "dev.to");
        assert!(!p.is_empty());
        let path = std::env::temp_dir().join("zalo-gpui-links-test.json");
        let mut map = HashMap::new();
        map.insert("https://dev.to/a".to_owned(), p.clone());
        save(&path, &map);
        assert_eq!(load(Some(&path))["https://dev.to/a"], p);
        assert_eq!(clock(3240), "0:04");
        assert_eq!(clock(65_000), "1:05");
    }
}
