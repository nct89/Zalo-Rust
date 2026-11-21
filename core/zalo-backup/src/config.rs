//! Remote configuration for PC → Phone Message Backup (`backup_msg` in `compact3197.js` /
//! `compact1115.js`) and server config cache from socket `634`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default auto-resume backoff table (`u = [2000, 8000, 30000]` in `compact3197.js`).
pub const DEFAULT_AUTO_RESUME_BACKOFF_MS: [u64; 3] = [2_000, 8_000, 30_000];

/// `oy = 1740000` ms (29 minutes) upload/630 session reuse window in `main-startup`.
pub const UPLOAD_REUSE_WINDOW_MS: u64 = 1_740_000;

/// Exact port of `secondsToMsWithSentinel`: preserves `-1` (disabled auto-hide sentinel),
/// otherwise converts seconds to milliseconds.
#[must_use]
pub fn seconds_to_ms_with_sentinel(seconds: i64) -> i64 {
    if seconds == -1 {
        -1
    } else if seconds <= 0 {
        0
    } else {
        seconds.saturating_mul(1_000)
    }
}

/// All 21 configuration keys from Electron `compact3197.js` object `b` (`backup_msg`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRemoteConfig {
    /// Feature gate (`enabled`, default `false`).
    pub enabled: bool,
    /// Spam rule max requests within `fixed_time` seconds (default `6`; 7th locks).
    pub max_request: u32,
    /// Spam window in seconds (default `60`).
    pub fixed_time: u64,
    /// Spam lock duration in seconds (default `120`).
    pub restrict_time: u64,
    /// Post-success cooldown in seconds (default `600`, multiplied by 1000).
    pub create_backup_waiting_time: u64,
    /// Optional max upload file size in bytes (default `0` = unlimited).
    pub max_file_size: u64,
    /// HTTP multipart chunk size in bytes (default `3_145_728` = 3 MiB).
    pub chunk_size: usize,
    /// UI percentage weight allocated to the packing phase (default `90`).
    pub backup_progress_weight: u32,
    /// Auto-hide duration for AfterSuccess banner in seconds (`-1` disables; default `300`).
    pub after_success_auto_hide_banner_seconds: i64,
    /// Auto-hide duration for Fail banner in seconds (`-1` disables; default `300`).
    pub after_fail_auto_hide_banner_seconds: i64,
    /// Auto-hide duration for Interrupted banner in seconds (`-1` disables; default `300`).
    pub after_interrupt_auto_hide_banner_seconds: i64,
    /// Banner hold time after system sleep resume in seconds (default `3`).
    pub sleep_resume_banner_hold_seconds: i64,
    /// Timeout in seconds for QoS-only `backup_done` monitor (default `180`).
    pub backup_done_timeout_seconds: u64,
    /// Auto-resume eligibility window in seconds (default `300`).
    pub auto_resume_window_seconds: u64,
    /// Maximum auto-resume attempts (default `3`).
    pub auto_resume_max_attempts: u32,
    /// Backoff schedule in milliseconds per auto-resume attempt (default `[2000, 8000, 30000]`).
    pub auto_resume_backoff_ms: Vec<u64>,
    /// Fallback backup TTL in ms when server `634` does not provide `retention_time` (default 3 days).
    pub backup_ttl_ms: u64,
    /// Whether backup blocks are encrypted via `libzproto_backup_wasm_bg` (default `true`).
    pub encryption_enabled: bool,
    /// Cache TTL in ms for `634` configs (default `-1` = always stale, always fetch fresh `634`).
    pub backup_configs_cache_ttl_ms: i64,
    /// Crypto format version sent in `630` metadata (default `1`).
    pub crypto_version: u32,
    /// DB format version sent in `630` metadata (default `1`).
    pub db_format_version: u32,
}

impl Default for BackupRemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_request: 6,
            fixed_time: 60,
            restrict_time: 120,
            create_backup_waiting_time: 600,
            max_file_size: 0,
            chunk_size: 3_145_728,
            backup_progress_weight: 90,
            after_success_auto_hide_banner_seconds: 300,
            after_fail_auto_hide_banner_seconds: 300,
            after_interrupt_auto_hide_banner_seconds: 300,
            sleep_resume_banner_hold_seconds: 3,
            backup_done_timeout_seconds: 180,
            auto_resume_window_seconds: 300,
            auto_resume_max_attempts: 3,
            auto_resume_backoff_ms: DEFAULT_AUTO_RESUME_BACKOFF_MS.to_vec(),
            backup_ttl_ms: 259_200_000,
            encryption_enabled: true,
            backup_configs_cache_ttl_ms: -1,
            crypto_version: 1,
            db_format_version: 1,
        }
    }
}

impl BackupRemoteConfig {
    /// Parses `backup_msg` overrides from `getServerInfo` / `getLoginInfo` JSON.
    /// Checks `setttings.features.backup_msg`, `settings.features.backup_msg`,
    /// `data.setttings.features.backup_msg`, `data.settings.features.backup_msg`,
    /// `features.backup_msg`, or a direct `backup_msg` object.
    #[must_use]
    pub fn from_server_info(root: &Value) -> Self {
        let mut cfg = Self::default();
        if let Some(node) = find_backup_feature(root) {
            cfg.apply_overlay(node);
        } else {
            cfg.apply_overlay(root);
        }
        cfg
    }

    /// Layers `features.backup_msg` from a `getServerInfo` or `getLoginInfo` JSON value
    /// onto this config if present, preserving existing values for omitted keys.
    pub fn apply_server_info(&mut self, root: &Value) {
        if let Some(node) = find_backup_feature(root) {
            self.apply_overlay(node);
        }
    }

    /// Applies a `backup_msg` JSON object overlay onto this config.
    pub fn apply_overlay(&mut self, node: &Value) {
        let Some(obj) = node.as_object() else {
            return;
        };

        if let Some(v) = obj.get("enabled").and_then(parse_bool) {
            self.enabled = v;
        }
        if let Some(v) = obj.get("max_request").and_then(parse_u64) {
            self.max_request = v as u32;
        }
        if let Some(v) = obj.get("fixed_time").and_then(parse_u64) {
            self.fixed_time = v;
        }
        if let Some(v) = obj.get("restrict_time").and_then(parse_u64) {
            self.restrict_time = v;
        }
        if let Some(v) = obj.get("create_backup_waiting_time").and_then(parse_u64) {
            self.create_backup_waiting_time = v;
        }
        if let Some(v) = obj.get("max_file_size").and_then(parse_u64) {
            self.max_file_size = v;
        }
        if let Some(v) = obj.get("chunk_size").and_then(parse_u64) {
            if v > 0 {
                self.chunk_size = v as usize;
            }
        }
        if let Some(v) = obj.get("backup_progress_weight").and_then(parse_u64) {
            self.backup_progress_weight = (v as u32).min(100);
        }
        if let Some(v) = obj
            .get("after_success_auto_hide_banner_seconds")
            .and_then(parse_i64)
        {
            self.after_success_auto_hide_banner_seconds = v;
        }
        if let Some(v) = obj
            .get("after_fail_auto_hide_banner_seconds")
            .and_then(parse_i64)
        {
            self.after_fail_auto_hide_banner_seconds = v;
        }
        if let Some(v) = obj
            .get("after_interrupt_auto_hide_banner_seconds")
            .and_then(parse_i64)
        {
            self.after_interrupt_auto_hide_banner_seconds = v;
        }
        if let Some(v) = obj
            .get("sleep_resume_banner_hold_seconds")
            .and_then(parse_i64)
        {
            self.sleep_resume_banner_hold_seconds = v;
        }
        if let Some(v) = obj.get("backup_done_timeout_seconds").and_then(parse_u64) {
            self.backup_done_timeout_seconds = v;
        }
        if let Some(v) = obj.get("auto_resume_window_seconds").and_then(parse_u64) {
            self.auto_resume_window_seconds = v;
        }
        if let Some(v) = obj.get("auto_resume_max_attempts").and_then(parse_u64) {
            self.auto_resume_max_attempts = v as u32;
        }
        if let Some(arr) = obj.get("auto_resume_backoff_ms").and_then(Value::as_array) {
            let parsed: Vec<u64> = arr.iter().filter_map(parse_u64).collect();
            if !parsed.is_empty() {
                self.auto_resume_backoff_ms = parsed;
            }
        }
        if let Some(v) = obj.get("backup_ttl_ms").and_then(parse_u64) {
            self.backup_ttl_ms = v;
        }
        if let Some(v) = obj.get("encryption_enabled").and_then(parse_bool) {
            self.encryption_enabled = v;
        }
        if let Some(v) = obj.get("backup_configs_cache_ttl_ms").and_then(parse_i64) {
            self.backup_configs_cache_ttl_ms = v;
        }
        if let Some(v) = obj.get("crypto_version").and_then(parse_u64) {
            self.crypto_version = v as u32;
        }
        if let Some(v) = obj.get("db_format_version").and_then(parse_u64) {
            self.db_format_version = v as u32;
        }
    }

    /// Cooldown in milliseconds (`create_backup_waiting_time * 1000`).
    #[must_use]
    pub fn cooldown_ms(&self) -> u64 {
        self.create_backup_waiting_time.saturating_mul(1_000)
    }

    /// Effective TTL in ms, preferring positive `retention_time` from socket `634` if present.
    #[must_use]
    pub fn effective_backup_ttl_ms(&self, server_configs: Option<&ServerBackupConfigs>) -> u64 {
        if let Some(sc) = server_configs {
            if sc.retention_time_ms > 0 {
                return sc.retention_time_ms;
            }
        }
        self.backup_ttl_ms
    }

    /// Returns whether cached `634` configs fetched at `fetched_at_ms` are still fresh.
    /// When `backup_configs_cache_ttl_ms < 0` (the default `-1`), ALWAYS returns `false`
    /// so every click fetches fresh `634` configs (matching Electron).
    #[must_use]
    pub fn is_server_configs_fresh(&self, fetched_at_ms: Option<u64>, now_ms: u64) -> bool {
        if self.backup_configs_cache_ttl_ms < 0 {
            return false;
        }
        let Some(fetched) = fetched_at_ms else {
            return false;
        };
        now_ms.saturating_sub(fetched) <= self.backup_configs_cache_ttl_ms as u64
    }

    /// Auto-hide ms for AfterSuccess banner (`-1` = do not auto-hide).
    #[must_use]
    pub fn after_success_auto_hide_ms(&self) -> i64 {
        seconds_to_ms_with_sentinel(self.after_success_auto_hide_banner_seconds)
    }

    /// Auto-hide ms for Fail banner (`-1` = do not auto-hide).
    #[must_use]
    pub fn after_fail_auto_hide_ms(&self) -> i64 {
        seconds_to_ms_with_sentinel(self.after_fail_auto_hide_banner_seconds)
    }

    /// Auto-hide ms for Interrupted banner (`-1` = do not auto-hide).
    #[must_use]
    pub fn after_interrupt_auto_hide_ms(&self) -> i64 {
        seconds_to_ms_with_sentinel(self.after_interrupt_auto_hide_banner_seconds)
    }
}

/// Parsed response of socket `634` (`GET_BACKUP_CONFIGS`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerBackupConfigs {
    /// Server-provided retention period in milliseconds (if `> 0`, overrides `backup_ttl_ms`).
    pub retention_time_ms: u64,
    /// Optional server-provided chunk size or max file size metadata.
    pub raw: Option<Value>,
}

fn find_backup_feature(root: &Value) -> Option<&Value> {
    root.pointer("/setttings/features/backup_msg")
        .or_else(|| root.pointer("/settings/features/backup_msg"))
        .or_else(|| root.pointer("/data/setttings/features/backup_msg"))
        .or_else(|| root.pointer("/data/settings/features/backup_msg"))
        .or_else(|| root.pointer("/features/backup_msg"))
        .or_else(|| root.get("backup_msg"))
        .filter(|v| v.is_object())
}

fn parse_bool(v: &Value) -> Option<bool> {
    if let Some(b) = v.as_bool() {
        return Some(b);
    }
    if let Some(n) = v.as_i64() {
        return Some(n != 0);
    }
    v.as_str().and_then(|s| match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    })
}

fn parse_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
}

fn parse_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
}
