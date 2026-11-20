//! Stable device identity used by every request.
//!
//! Zalo ties the QR session to `imei`, `first_launch_time` and
//! `computer_name`. They are persisted so a restart does not look like a
//! new device.

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// The identity sent to Zalo on every unauthenticated request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// 24 lowercase hex characters.
    pub imei: String,
    /// UNIX seconds at the first launch.
    pub first_launch_time: u64,
    /// Host name reported as `computer_name`.
    pub computer_name: String,
}

/// Why a device identity could not be read or written.
#[derive(Debug, Error)]
pub enum DeviceError {
    /// Reading or writing the device file failed.
    #[error("device I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The device file is not valid JSON or misses a field.
    #[error("device JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

impl Device {
    /// Loads `device.json` from `dir`, creating it on first run.
    ///
    /// # Errors
    ///
    /// Returns [`DeviceError`] when the file cannot be read or written.
    pub fn load_or_create(dir: &Path) -> Result<Self, DeviceError> {
        let path = device_path(dir);
        if path.exists() {
            let raw = fs::read_to_string(&path)?;
            return Ok(serde_json::from_str(&raw)?);
        }
        let device = Self::generate();
        fs::create_dir_all(dir)?;
        fs::write(&path, serde_json::to_vec_pretty(&device)?)?;
        Ok(device)
    }

    /// Generates a fresh identity for this machine.
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let imei: String = (0..24)
            .map(|_| {
                std::char::from_digit(rng.gen_range(0..16), 16).expect("hex digit is always valid")
            })
            .collect();
        Self {
            imei,
            first_launch_time: unix_now(),
            computer_name: hostname(),
        }
    }
}

/// The path of the persisted device record inside `dir`.
pub fn device_path(dir: &Path) -> PathBuf {
    dir.join("device.json")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn hostname() -> String {
    if let Ok(name) = fs::read_to_string("/proc/sys/kernel/hostname") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_owned();
        }
    }
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "linux".to_owned())
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.computer_name, self.imei)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn generated_identity_has_the_expected_shape() {
        let device = Device::generate();
        assert_eq!(device.imei.len(), 24);
        assert!(device.imei.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!device.computer_name.is_empty());
        assert!(device.first_launch_time > 0);
    }

    #[test]
    fn load_or_create_is_stable_across_runs() {
        let temp = TempDir::new("device");
        let first = Device::load_or_create(temp.path()).expect("create");
        let second = Device::load_or_create(temp.path()).expect("load");
        assert_eq!(first, second);
        assert!(device_path(temp.path()).exists());
    }
}
