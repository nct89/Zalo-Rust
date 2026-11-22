//! Linux hardware fingerprint (`hwid.js` parity — `zmh1-linux`).
//!
//! Computes `sha256("zmh1-linux\n{uuid}\n{mid}\n{vol}")` and caches non-empty
//! components in `~/.config/ZaloMulti/hwid-parts.json` (`0600`) so transient
//! `findmnt` or DMI permission differences never shift the machine ID.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths::app_data_dir;

/// Raw hardware fingerprint parts persisted in `hwid-parts.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HwidParts {
    /// `/sys/class/dmi/id/product_uuid` (uppercase, empty if unreadable/placeholder).
    #[serde(default)]
    pub uuid: String,
    /// `/etc/machine-id` or `/var/lib/dbus/machine-id` (uppercase).
    #[serde(default)]
    pub mid: String,
    /// Root filesystem UUID via `findmnt -no UUID /` (uppercase).
    #[serde(default)]
    pub vol: String,
}

fn read_trimmed(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

fn system_uuid() -> String {
    let uuid = read_trimmed("/sys/class/dmi/id/product_uuid").to_ascii_uppercase();
    if uuid.is_empty()
        || uuid == "00000000-0000-0000-0000-000000000000"
        || uuid == "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF"
    {
        String::new()
    } else {
        uuid
    }
}

fn machine_id() -> String {
    let primary = read_trimmed("/etc/machine-id");
    if !primary.is_empty() {
        primary
    } else {
        read_trimmed("/var/lib/dbus/machine-id")
    }
}

fn root_volume_uuid() -> String {
    Command::new("findmnt")
        .args(["-no", "UUID", "/"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// Merge newly read parts over cached parts: fresh non-empty wins, empty preserves saved.
#[must_use]
pub fn merge_parts(saved: &HwidParts, fresh: &HwidParts) -> HwidParts {
    let pick = |s: &str, f: &str| {
        let next = f.trim().to_uppercase();
        if !next.is_empty() {
            next
        } else {
            s.trim().to_uppercase()
        }
    };
    HwidParts {
        uuid: pick(&saved.uuid, &fresh.uuid),
        mid: pick(&saved.mid, &fresh.mid),
        vol: pick(&saved.vol, &fresh.vol),
    }
}

/// Exact pre-hash string (`zmh1-linux\n{uuid}\n{mid}\n{vol}`).
#[must_use]
pub fn material_of(parts: &HwidParts) -> String {
    format!("zmh1-linux\n{}\n{}\n{}", parts.uuid, parts.mid, parts.vol)
}

fn compute_hwid_in(app_dir: &Path) -> String {
    let parts_file = app_dir.join("hwid-parts.json");
    let saved: HwidParts = std::fs::read_to_string(&parts_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let fresh = HwidParts {
        uuid: system_uuid(),
        mid: machine_id(),
        vol: root_volume_uuid(),
    };
    let parts = merge_parts(&saved, &fresh);
    if parts != saved {
        if let Ok(json) = serde_json::to_string(&parts) {
            if std::fs::write(&parts_file, json).is_ok() {
                let _ = std::fs::set_permissions(&parts_file, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    let digest = Sha256::digest(material_of(&parts).as_bytes());
    hex::encode(digest)
}

/// Process-cached 64-character lowercase hex Linux HWID.
#[must_use]
pub fn cached_hwid() -> &'static str {
    static HWID: OnceLock<String> = OnceLock::new();
    HWID.get_or_init(|| compute_hwid_in(&app_data_dir()))
}

/// Display format: first 24 hex chars uppercased in 4 groups of 6 (`XXXXXX-XXXXXX-XXXXXX-XXXXXX`).
#[must_use]
pub fn format_hwid(hwid: &str) -> String {
    if hwid.len() != 64 || !hwid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return String::new();
    }
    let up = hwid[..24].to_ascii_uppercase();
    format!("{}-{}-{}-{}", &up[0..6], &up[6..12], &up[12..18], &up[18..24])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_parts_without_losing_cached_components() {
        let saved = HwidParts {
            uuid: "ABC".into(),
            mid: "MID".into(),
            vol: "VOL".into(),
        };
        let kept = merge_parts(
            &saved,
            &HwidParts {
                uuid: String::new(),
                mid: "mid".into(),
                vol: String::new(),
            },
        );
        assert_eq!(kept, saved);
        assert_eq!(material_of(&kept), "zmh1-linux\nABC\nMID\nVOL");

        let upgraded = merge_parts(
            &HwidParts {
                uuid: String::new(),
                mid: "MID".into(),
                vol: String::new(),
            },
            &HwidParts {
                uuid: "NEW".into(),
                mid: String::new(),
                vol: "DISK".into(),
            },
        );
        assert_eq!(
            upgraded,
            HwidParts {
                uuid: "NEW".into(),
                mid: "MID".into(),
                vol: "DISK".into(),
            }
        );
    }

    #[test]
    fn formats_hwid_into_four_six_char_groups() {
        assert_eq!(
            format_hwid("80ef55268af64ab5ddcbb9556cd2c99fafbbf2b260d6e622fda7098a5a61be23"),
            "80EF55-268AF6-4AB5DD-CBB955"
        );
        assert_eq!(format_hwid("xyz"), "");
    }
}
