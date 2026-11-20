//! Settings persistence.
//!
//! Keys that `zalo-gui` also understands (`dark`, `followSystem`, `lang`,
//! `notifyOff`, `split`, `hideTyping`) live in `~/.config/zalo-rust/ui-state.json`
//! and are merged in place so the egui client keeps its other fields.
//! GPUI-only keys go to `gpui-prefs.json` (zalo-gui rewrites ui-state.json
//! wholesale and would drop them). Tests never touch the disk.

use std::path::PathBuf;

use serde_json::{Map, Value};

/// Keys shared with `zalo-gui`'s `ui-state.json`.
const SHARED: [&str; 12] = [
    "dark",
    "followSystem",
    "lang",
    "notifyOff",
    "split",
    "hideTyping",
    "pinned",
    "muted",
    "other",
    "read",
    "quick",
    "quickOff",
];

/// In-memory settings with write-through to disk.
#[derive(Clone, Debug, Default)]
pub struct Prefs {
    dir: Option<PathBuf>,
    shared: Map<String, Value>,
    own: Map<String, Value>,
}

impl Prefs {
    /// Load from the XDG config dir (`None` in tests / without HOME).
    #[must_use]
    pub fn load(dir: Option<PathBuf>) -> Self {
        let read = |name: &str| -> Map<String, Value> {
            dir.as_ref()
                .and_then(|d| std::fs::read_to_string(d.join(name)).ok())
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default()
        };
        Self {
            shared: read("ui-state.json"),
            own: read("gpui-prefs.json"),
            dir,
        }
    }

    fn map(&self, key: &str) -> &Map<String, Value> {
        if SHARED.contains(&key) {
            &self.shared
        } else {
            &self.own
        }
    }

    /// Boolean value or `default`.
    #[must_use]
    pub fn bool(&self, key: &str, default: bool) -> bool {
        self.map(key)
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    }

    /// String value or `default`.
    #[must_use]
    pub fn str(&self, key: &str, default: &str) -> String {
        self.map(key)
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_owned()
    }

    /// Compact JSON of one key (`null` when unset).
    #[must_use]
    pub fn raw(&self, key: &str) -> String {
        self.map(key)
            .get(key)
            .map(Value::to_string)
            .unwrap_or_else(|| "null".into())
    }

    /// Cloned JSON value of `key`.
    #[must_use]
    pub fn json(&self, key: &str) -> Option<Value> {
        self.map(key).get(key).cloned()
    }

    /// Set and persist one key.
    pub fn set(&mut self, key: &str, value: Value) {
        let shared = SHARED.contains(&key);
        if shared {
            self.shared.insert(key.to_owned(), value);
        } else {
            self.own.insert(key.to_owned(), value);
        }
        self.save(shared);
    }

    fn save(&self, shared: bool) {
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let (name, map) = if shared {
            // Re-read so fields zalo-gui wrote meanwhile (read marks, pins) survive.
            let mut current = std::fs::read_to_string(dir.join("ui-state.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            for key in SHARED {
                if let Some(v) = self.shared.get(key) {
                    current.insert(key.to_owned(), v.clone());
                }
            }
            ("ui-state.json", current)
        } else {
            ("gpui-prefs.json", self.own.clone())
        };
        let Ok(text) = serde_json::to_string(&Value::Object(map)) else {
            return;
        };
        let tmp = dir.join(format!(".{name}.tmp"));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_and_own_keys_split() {
        let dir = std::env::temp_dir().join("zalo-gpui-prefs-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ui-state.json"),
            r#"{"read":{"a":1},"split":false}"#,
        )
        .unwrap();
        let mut p = Prefs::load(Some(dir.clone()));
        assert!(!p.bool("split", true));
        p.set("split", Value::Bool(true));
        p.set("dblReply", Value::Bool(true));
        let ui: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("ui-state.json")).unwrap())
                .unwrap();
        assert_eq!(ui["split"], Value::Bool(true));
        assert_eq!(ui["read"]["a"], 1, "zalo-gui fields kept");
        assert!(ui.get("dblReply").is_none());
        let own: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("gpui-prefs.json")).unwrap())
                .unwrap();
        assert_eq!(own["dblReply"], Value::Bool(true));
        let p2 = Prefs::load(Some(dir));
        assert!(p2.bool("dblReply", false));
        let mut p3 = Prefs::load(Some(std::env::temp_dir().join("zalo-gpui-prefs-test")));
        p3.set(
            "window",
            serde_json::json!({"w": 1280, "h": 800, "max": false}),
        );
        let w = p3.json("window").unwrap();
        assert_eq!(w["w"], 1280);
        assert_eq!(w["h"], 800);
    }
}
