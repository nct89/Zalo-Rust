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

/// Account-scoped keys stored in `ui-state.json` inside the account directory.
const ACCOUNT_SHARED: [&str; 6] = ["pinned", "muted", "other", "read", "quick", "quickOff"];

/// Account-scoped keys stored in `gpui-prefs.json` inside the account directory.
const ACCOUNT_OWN: [&str; 12] = [
    "archived",
    "hidden",
    "removed",
    "hiddenPin",
    "lockHash",
    "receipts",
    "labels",
    "convLabels",
    "drafts",
    "readAt",
    "aliases",
    "starredMsgs",
];

fn is_account_key(key: &str) -> bool {
    ACCOUNT_SHARED.contains(&key) || ACCOUNT_OWN.contains(&key)
}

/// In-memory settings with write-through to disk.
///
/// App-wide settings (theme, language, notifications, window bounds, audio)
/// live in `machine_dir`. Account-scoped state (pins, mutes, archived/hidden
/// threads, read marks, screen lock, starred messages, receipts) lives in
/// `account_dir` so logging into different accounts never mixes state.
#[derive(Clone, Debug, Default)]
pub struct Prefs {
    dir: Option<PathBuf>,
    account_dir: Option<PathBuf>,
    shared: Map<String, Value>,
    own: Map<String, Value>,
    account_shared: Map<String, Value>,
    account_own: Map<String, Value>,
}

fn read_json_map(dir: Option<&PathBuf>, name: &str) -> Map<String, Value> {
    dir.and_then(|d| std::fs::read_to_string(d.join(name)).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

impl Prefs {
    /// Load from a single directory (`None` in tests / without HOME).
    #[must_use]
    pub fn load(dir: Option<PathBuf>) -> Self {
        Self::load_scoped(dir.clone(), dir)
    }

    /// Load machine-wide prefs from `machine_dir` and account-scoped prefs from
    /// `account_dir` (`accounts/<userId>`). When `account_dir` is `None` or
    /// equals `machine_dir`, both scopes share `machine_dir`.
    #[must_use]
    pub fn load_scoped(machine_dir: Option<PathBuf>, account_dir: Option<PathBuf>) -> Self {
        let shared = read_json_map(machine_dir.as_ref(), "ui-state.json");
        let own = read_json_map(machine_dir.as_ref(), "gpui-prefs.json");
        let separate = account_dir.is_some() && account_dir != machine_dir;
        let (account_shared, account_own) = if separate {
            (
                read_json_map(account_dir.as_ref(), "ui-state.json"),
                read_json_map(account_dir.as_ref(), "gpui-prefs.json"),
            )
        } else {
            (shared.clone(), own.clone())
        };
        Self {
            dir: machine_dir,
            account_dir,
            shared,
            own,
            account_shared,
            account_own,
        }
    }

    fn separate_account(&self) -> bool {
        self.account_dir.is_some() && self.account_dir != self.dir
    }

    fn map(&self, key: &str) -> &Map<String, Value> {
        let shared = SHARED.contains(&key);
        if self.separate_account() && is_account_key(key) {
            if shared {
                &self.account_shared
            } else {
                &self.account_own
            }
        } else if shared {
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
        if self.separate_account() && is_account_key(key) {
            if shared {
                self.account_shared.insert(key.to_owned(), value);
            } else {
                self.account_own.insert(key.to_owned(), value);
            }
            self.save_in(self.account_dir.as_ref(), shared, true);
            return;
        }
        if shared {
            self.shared.insert(key.to_owned(), value);
        } else {
            self.own.insert(key.to_owned(), value);
        }
        self.save_in(self.dir.as_ref(), shared, false);
    }

    /// Removes account-scoped keys from `ui-state.json` and `gpui-prefs.json`
    /// while preserving device/app settings (theme, language, window size, audio devices).
    #[cfg(test)]
    pub fn reset_account_keys(&mut self) {
        for k in ACCOUNT_SHARED {
            self.shared.remove(k);
            self.account_shared.remove(k);
        }
        for k in ACCOUNT_OWN {
            self.own.remove(k);
            self.account_own.remove(k);
        }
        let target = if self.separate_account() {
            self.account_dir.clone()
        } else {
            self.dir.clone()
        };
        let Some(dir) = target.as_ref() else {
            return;
        };
        let mut current = std::fs::read_to_string(dir.join("ui-state.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        for k in ACCOUNT_SHARED {
            current.remove(k);
        }
        if let Ok(text) = serde_json::to_string(&Value::Object(current)) {
            let tmp = dir.join(".ui-state.json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, dir.join("ui-state.json"));
            }
        }
        self.save_in(Some(dir), false, self.separate_account());
    }

    fn save_in(&self, dir: Option<&PathBuf>, shared: bool, account_scope: bool) {
        let Some(dir) = dir else {
            return;
        };
        let _ = std::fs::create_dir_all(dir);
        let (name, map) = if shared {
            // Re-read so fields zalo-gui wrote meanwhile (read marks, pins) survive.
            let mut current = std::fs::read_to_string(dir.join("ui-state.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let src = if account_scope {
                &self.account_shared
            } else {
                &self.shared
            };
            let keys: &[&str] = if account_scope {
                &ACCOUNT_SHARED
            } else {
                &SHARED
            };
            for key in keys {
                if let Some(v) = src.get(*key) {
                    current.insert((*key).to_owned(), v.clone());
                }
            }
            ("ui-state.json", current)
        } else {
            let src = if account_scope {
                &self.account_own
            } else {
                &self.own
            };
            ("gpui-prefs.json", src.clone())
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

    #[test]
    fn scoped_prefs_isolate_account_keys_across_accounts() {
        let root = std::env::temp_dir().join(format!(
            "zalo-gpui-prefs-scoped-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let machine = root.join("machine");
        let acct_a = machine.join("accounts").join("111");
        let acct_b = machine.join("accounts").join("222");
        std::fs::create_dir_all(&acct_a).unwrap();
        std::fs::create_dir_all(&acct_b).unwrap();

        let mut pa = Prefs::load_scoped(Some(machine.clone()), Some(acct_a.clone()));
        pa.set("dark", Value::Bool(true));
        pa.set("pinned", serde_json::json!(["t1"]));
        pa.set("other", serde_json::json!(["t2"]));
        pa.set("lockHash", Value::String("secret-hash".into()));
        pa.set("starredMsgs", serde_json::json!([1, 2, 3]));

        // Switching to account B keeps machine theme, but never leaks account A's pins/lock/stars.
        let mut pb = Prefs::load_scoped(Some(machine.clone()), Some(acct_b.clone()));
        assert!(pb.bool("dark", false));
        assert!(pb.json("pinned").is_none());
        assert!(pb.json("other").is_none());
        assert_eq!(pb.str("lockHash", ""), "");
        assert!(pb.json("starredMsgs").is_none());

        pb.set("pinned", serde_json::json!(["b1"]));

        // Returning to account A preserves its own keys untouched.
        let pa2 = Prefs::load_scoped(Some(machine.clone()), Some(acct_a));
        assert_eq!(pa2.json("pinned"), Some(serde_json::json!(["t1"])));
        assert_eq!(pa2.str("lockHash", ""), "secret-hash");
        assert_eq!(pa2.json("starredMsgs"), Some(serde_json::json!([1, 2, 3])));

        let _ = std::fs::remove_dir_all(&root);
    }
}
