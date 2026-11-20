//! Runtime-overridable Zalo protocol parameters (`protocol.json` + env vars).
//!
//! When Zalo ships a new desktop release that bumps `zpw_ver` (`client_version`),
//! `User-Agent`, or the `ParamCipher` key, users and packagers can drop an
//! updated `protocol.json` into `$XDG_CONFIG_HOME/zalo-rust/protocol.json` (or
//! pass `ZALO_CLIENT_VERSION`, `ZALO_API_TYPE`, `ZALO_USER_AGENT`,
//! `ZALO_PARAM_CIPHER_KEY`) without rebuilding the binary.

use crate::param_cipher::{API_TYPE, CLIENT_VERSION, PARAM_CIPHER_KEY};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{OnceLock, RwLock};
use thiserror::Error;

/// Default User-Agent string matching the pinned Zalo Desktop release.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Zalo/26.10.20";

/// Default pinned Zalo Desktop release version tracked by this repository.
pub const DEFAULT_ZALO_DESKTOP_VERSION: &str = "26.10.20";

/// File name searched inside the user config directory (`$XDG_CONFIG_HOME/zalo-rust/`).
pub const PROTOCOL_CONFIG_FILE: &str = "protocol.json";

/// Active protocol configuration used by HTTP, WebSocket, and `ParamCipher`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolConfig {
    /// Sent as `client_version` and `zpw_ver`.
    pub client_version: String,
    /// Sent as `type` and `zpw_type`.
    pub api_type: u32,
    /// HTTP/WebSocket `User-Agent` header.
    pub user_agent: String,
    /// 32-byte ASCII/raw key used by `ParamCipher` v2.
    pub param_cipher_key: [u8; 32],
    /// Informative Zalo Desktop release version corresponding to these values.
    pub zalo_desktop_version: String,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            client_version: CLIENT_VERSION.to_owned(),
            api_type: API_TYPE,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            param_cipher_key: *PARAM_CIPHER_KEY,
            zalo_desktop_version: DEFAULT_ZALO_DESKTOP_VERSION.to_owned(),
        }
    }
}

/// Partial JSON overrides loaded from `protocol.json`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProtocolOverrides {
    /// Override for `client_version` / `zpw_ver` (e.g. `"694"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    /// Override for `type` / `zpw_type` (default `30`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_type: Option<u32>,
    /// Override for HTTP/WebSocket `User-Agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Override for the 32-byte `ParamCipher` key (exact 32 ASCII bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_cipher_key: Option<String>,
    /// Reference Zalo Desktop version string (e.g. `"26.10.20"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zalo_desktop_version: Option<String>,
}

/// Errors produced when loading or validating protocol overrides.
#[derive(Debug, Error)]
pub enum ProtocolConfigError {
    /// Reading `protocol.json` failed.
    #[error("failed to read protocol config: {0}")]
    Io(#[from] std::io::Error),
    /// Parsing `protocol.json` failed.
    #[error("invalid protocol.json: {0}")]
    Json(#[from] serde_json::Error),
    /// `param_cipher_key` must be exactly 32 bytes.
    #[error("param_cipher_key must be exactly 32 bytes (got {0})")]
    InvalidParamCipherKey(usize),
    /// `ZALO_API_TYPE` did not parse as a `u32`.
    #[error("invalid ZALO_API_TYPE value: {0}")]
    InvalidApiType(String),
}

fn store() -> &'static RwLock<ProtocolConfig> {
    static CONFIG: OnceLock<RwLock<ProtocolConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| RwLock::new(ProtocolConfig::default()))
}

/// Returns a snapshot of the active protocol configuration.
#[must_use]
pub fn active_config() -> ProtocolConfig {
    store()
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|e| e.into_inner().clone())
}

/// Active `client_version` / `zpw_ver` value.
#[must_use]
pub fn client_version() -> String {
    active_config().client_version
}

/// Active `api_type` / `zpw_type` value.
#[must_use]
pub fn api_type() -> u32 {
    active_config().api_type
}

/// Active HTTP/WebSocket `User-Agent`.
#[must_use]
pub fn user_agent() -> String {
    active_config().user_agent
}

/// Active 32-byte `ParamCipher` key.
#[must_use]
pub fn param_cipher_key() -> [u8; 32] {
    active_config().param_cipher_key
}

/// Applies non-empty fields from `overrides` onto the active configuration.
///
/// # Errors
///
/// Returns [`ProtocolConfigError::InvalidParamCipherKey`] if `param_cipher_key`
/// is provided and is not exactly 32 bytes long.
pub fn apply_overrides(
    overrides: &ProtocolOverrides,
) -> Result<ProtocolConfig, ProtocolConfigError> {
    let parsed_key = match &overrides.param_cipher_key {
        Some(raw) if !raw.trim().is_empty() => {
            let bytes = raw.trim().as_bytes();
            let arr: [u8; 32] = bytes
                .try_into()
                .map_err(|_| ProtocolConfigError::InvalidParamCipherKey(bytes.len()))?;
            Some(arr)
        }
        _ => None,
    };

    let mut guard = store().write().unwrap_or_else(|e| e.into_inner());
    if let Some(ver) = &overrides.client_version {
        let trimmed = ver.trim();
        if !trimmed.is_empty() {
            guard.client_version = trimmed.to_owned();
        }
    }
    if let Some(api) = overrides.api_type {
        guard.api_type = api;
    }
    if let Some(ua) = &overrides.user_agent {
        let trimmed = ua.trim();
        if !trimmed.is_empty() {
            guard.user_agent = trimmed.to_owned();
        }
    }
    if let Some(key) = parsed_key {
        guard.param_cipher_key = key;
    }
    if let Some(desktop) = &overrides.zalo_desktop_version {
        let trimmed = desktop.trim();
        if !trimmed.is_empty() {
            guard.zalo_desktop_version = trimmed.to_owned();
        }
    }
    Ok(guard.clone())
}

/// Loads `protocol.json` from `path` if it exists and merges it into the active config.
///
/// Returns `Ok(None)` when the file does not exist.
///
/// # Errors
///
/// Returns [`ProtocolConfigError`] if the file exists but cannot be read,
/// parsed, or validated.
pub fn load_overrides_from_file(
    path: &Path,
) -> Result<Option<ProtocolConfig>, ProtocolConfigError> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    let overrides: ProtocolOverrides = serde_json::from_str(&raw)?;
    apply_overrides(&overrides).map(Some)
}

/// Applies environment variable overrides (`ZALO_CLIENT_VERSION`, `ZALO_API_TYPE`,
/// `ZALO_USER_AGENT`, `ZALO_PARAM_CIPHER_KEY`) onto the active configuration.
///
/// # Errors
///
/// Returns [`ProtocolConfigError`] when `ZALO_API_TYPE` or `ZALO_PARAM_CIPHER_KEY`
/// has an invalid value.
pub fn apply_env_overrides() -> Result<ProtocolConfig, ProtocolConfigError> {
    let api_type = match std::env::var("ZALO_API_TYPE") {
        Ok(val) if !val.trim().is_empty() => Some(
            val.trim()
                .parse::<u32>()
                .map_err(|_| ProtocolConfigError::InvalidApiType(val))?,
        ),
        _ => None,
    };
    let overrides = ProtocolOverrides {
        client_version: std::env::var("ZALO_CLIENT_VERSION").ok(),
        api_type,
        user_agent: std::env::var("ZALO_USER_AGENT").ok(),
        param_cipher_key: std::env::var("ZALO_PARAM_CIPHER_KEY").ok(),
        zalo_desktop_version: std::env::var("ZALO_DESKTOP_VERSION").ok(),
    };
    apply_overrides(&overrides)
}

/// Initializes protocol overrides from `<config_dir>/protocol.json` (if present),
/// followed by environment variables (which take precedence).
///
/// Invalid files or env vars are logged to `stderr` and leave built-in defaults intact.
pub fn init_from_dir(config_dir: &Path) -> ProtocolConfig {
    let path = config_dir.join(PROTOCOL_CONFIG_FILE);
    if let Err(err) = load_overrides_from_file(&path) {
        eprintln!("zalo-protocol: ignoring {}: {err}", path.display());
    }
    if let Err(err) = apply_env_overrides() {
        eprintln!("zalo-protocol: ignoring env override: {err}");
    }
    active_config()
}

/// Restores compiled-in defaults (primarily useful in tests).
pub fn reset_defaults() {
    let mut guard = store().write().unwrap_or_else(|e| e.into_inner());
    *guard = ProtocolConfig::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_match_compiled_constants() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_defaults();
        let cfg = active_config();
        assert_eq!(cfg.client_version, CLIENT_VERSION);
        assert_eq!(cfg.api_type, API_TYPE);
        assert_eq!(cfg.user_agent, DEFAULT_USER_AGENT);
        assert_eq!(&cfg.param_cipher_key, PARAM_CIPHER_KEY);
    }

    #[test]
    fn partial_json_file_overrides_only_specified_fields() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_defaults();
        let dir = std::env::temp_dir().join(format!("zalo-proto-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(PROTOCOL_CONFIG_FILE);
        std::fs::write(
            &path,
            r#"{"client_version": "701", "zalo_desktop_version": "26.12.01"}"#,
        )
        .unwrap();

        let loaded = load_overrides_from_file(&path).unwrap().unwrap();
        assert_eq!(loaded.client_version, "701");
        assert_eq!(loaded.zalo_desktop_version, "26.12.01");
        assert_eq!(loaded.api_type, API_TYPE);
        assert_eq!(loaded.user_agent, DEFAULT_USER_AGENT);
        assert_eq!(client_version(), "701");

        let _ = std::fs::remove_dir_all(&dir);
        reset_defaults();
    }

    #[test]
    fn rejects_invalid_param_cipher_key_length() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_defaults();
        let err = apply_overrides(&ProtocolOverrides {
            param_cipher_key: Some("too-short".to_owned()),
            ..ProtocolOverrides::default()
        })
        .unwrap_err();
        assert!(matches!(err, ProtocolConfigError::InvalidParamCipherKey(9)));
        assert_eq!(&param_cipher_key(), PARAM_CIPHER_KEY);
    }
}
