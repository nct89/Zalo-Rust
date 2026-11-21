//! Remote config for Electron's `sync_2_pc` namespace (`jae` / `Uae` / `zae`
//! in `main-startup.fe4e52efd649575b333b.js`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JavaScript `Number.MAX_SAFE_INTEGER` (`9_007_199_254_740_991`).
pub const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Unlimited limit sentinel `mo` (`2_147_483_647`) used by `PrepareAll` and
/// non-positive query limits in Electron's `shared-worker`.
pub const SYNC_UNLIMITED_LIMIT: u64 = 2_147_483_647;

/// Sync source / mode (Electron `SyncSource`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncSource {
    /// Manual sync triggered from Settings or banner.
    Manual,
    /// Auto sync triggered after login via valid `transfer_after_login` tempKey.
    AutoSync,
}

/// Time range `[from_ts, to_ts]` in Unix milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    /// Inclusive lower timestamp bound (ms).
    pub from_ts: u64,
    /// Inclusive upper timestamp bound (ms).
    pub to_ts: u64,
}

impl Default for TimeRange {
    fn default() -> Self {
        Self {
            from_ts: 0,
            to_ts: JS_MAX_SAFE_INTEGER,
        }
    }
}

/// Timeout configuration passed to `RemoteDatabase` / `SessionManager`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeoutConfigs {
    /// `awaiting_mobile_timeout_ms` (fallback `600_000` ms = 10 min, NOT 180s!).
    pub confirm_timeout_ms: u64,
    /// `wakeup_timeout` (fallback `180_000` ms = 3 min).
    pub wakeup_timeout_ms: u64,
    /// `batch_timeout` (fallback `10_000` ms = 10s).
    pub batch_timeout_ms: u64,
}

impl Default for TimeoutConfigs {
    fn default() -> Self {
        Self {
            confirm_timeout_ms: 600_000,
            wakeup_timeout_ms: 180_000,
            batch_timeout_ms: 10_000,
        }
    }
}

/// Per-branch (`manual_sync` or `auto_sync`) configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncBranchConfig {
    /// For `auto_sync`: `auto_sync.enable` (fallback `true`).
    pub enable: bool,
    /// `awaiting_mobile_timeout_ms` (fallback `600_000`).
    pub awaiting_mobile_timeout_ms: u64,
    /// `wakeup_timeout` (fallback `180_000`).
    pub wakeup_timeout: u64,
    /// `batch_timeout` (fallback `10_000`).
    pub batch_timeout: u64,
    /// `sync_conv_list.max_time_range_d` (fallback `-1`).
    pub conv_list_max_time_range_d: i64,
    /// `sync_conv_list.max_number_of_conv` (fallback `-1`).
    pub conv_list_max_number_of_conv: i64,
    /// `sync_last_chat.max_time_range_d` (fallback `-1`).
    pub last_chat_max_time_range_d: i64,
    /// `sync_last_chat.max_number_of_conv` (fallback `10`).
    pub last_chat_max_number_of_conv: i64,
    /// `sync_last_chat.max_number_of_msg_per_conv` (fallback `1`).
    pub last_chat_max_number_of_msg_per_conv: i64,
    /// `sync_recent_chat.max_time_range_d` (fallback `-1`).
    pub recent_chat_max_time_range_d: i64,
    /// `sync_recent_chat.max_number_of_conv` (fallback `10`).
    pub recent_chat_max_number_of_conv: i64,
    /// `sync_recent_chat.max_number_of_msg_per_conv` (fallback `100`).
    pub recent_chat_max_number_of_msg_per_conv: i64,
    /// `sync_last_and_recent_chat.max_time_range_d` (fallback `-1`).
    pub last_and_recent_max_time_range_d: i64,
    /// `sync_all_remaining_msg.max_time_range_d` (fallback `-1`).
    pub remaining_max_time_range_d: i64,
    /// `sync_all_remaining_msg.max_number_of_conv` (fallback `-1`).
    pub remaining_max_number_of_conv: i64,
    /// `sync_all_remaining_msg.max_number_of_msg_per_conv` (fallback `-1`).
    pub remaining_max_number_of_msg_per_conv: i64,
    /// `sync_all_remaining_msg.enable_success_noti_close_time_ms` (fallback `2_000`).
    pub close_success_timeout_ms: i64,
    /// `sync_all_remaining_msg.enable_failed_noti_close_time_ms` (fallback `30_000`).
    pub close_error_timeout_ms: i64,
    /// `sync_all_remaining_msg.enable_fail_noti_close_time_ms` (fallback `5_000`).
    pub close_fail_timeout_ms: i64,
    /// `sync_all_remaining_msg.enable_interrupt_noti_close_time_ms` (fallback `30_000`).
    pub close_interrupt_timeout_ms: i64,
}

impl Default for SyncBranchConfig {
    fn default() -> Self {
        Self {
            enable: true,
            awaiting_mobile_timeout_ms: 600_000,
            wakeup_timeout: 180_000,
            batch_timeout: 10_000,
            conv_list_max_time_range_d: -1,
            conv_list_max_number_of_conv: -1,
            last_chat_max_time_range_d: -1,
            last_chat_max_number_of_conv: 10,
            last_chat_max_number_of_msg_per_conv: 1,
            recent_chat_max_time_range_d: -1,
            recent_chat_max_number_of_conv: 10,
            recent_chat_max_number_of_msg_per_conv: 100,
            last_and_recent_max_time_range_d: -1,
            remaining_max_time_range_d: -1,
            remaining_max_number_of_conv: -1,
            remaining_max_number_of_msg_per_conv: -1,
            close_success_timeout_ms: 2_000,
            close_error_timeout_ms: 30_000,
            close_fail_timeout_ms: 5_000,
            close_interrupt_timeout_ms: 30_000,
        }
    }
}

/// Complete remote config for `features.sync_2_pc`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncRemoteConfig {
    /// `enable` (Electron default `false`; remote config turns on for eligible accounts).
    pub enable: bool,
    /// `enable_on_sync_source_setting` (default `true`).
    pub enable_on_sync_source_setting: bool,
    /// `batch_size` (default `2000`): per-conversation `GetAll` message limit.
    pub batch_size: u64,
    /// `conversations_batch_size` (default `1000`): conversation list page size.
    pub conversations_batch_size: u64,
    /// `transfer_batch_size` (default `2000`): `req.batchSize` rows per uploaded batch.
    pub transfer_batch_size: u64,
    /// `max_number_of_concurrent_sync_items` (default `4`, clamped 1..=5).
    pub max_number_of_concurrent_sync_items: usize,
    /// `skip_sync_preview_if_failed` (default `true`).
    pub skip_sync_preview_if_failed: bool,
    /// `max_percent_batch_fail` (default `0.5`).
    pub max_percent_batch_fail: f64,
    /// `max_batch_fail` (default `10`).
    pub max_batch_fail: usize,
    /// `sync_conv_list_batch_max_attempts` (default `3`).
    pub sync_conv_list_batch_max_attempts: u32,
    /// `sync_paused_timeout_ms` (default `600_000`).
    pub sync_paused_timeout_ms: u64,
    /// `sync_conv_list_update_ui_batch_size` (default `1`).
    pub sync_conv_list_update_ui_batch_size: usize,
    /// `manual_sync` nested branch (`Uae`).
    pub manual_sync: SyncBranchConfig,
    /// `auto_sync` nested branch (`zae`).
    pub auto_sync: SyncBranchConfig,
}

impl Default for SyncRemoteConfig {
    fn default() -> Self {
        Self {
            enable: false,
            enable_on_sync_source_setting: true,
            batch_size: 2_000,
            conversations_batch_size: 1_000,
            transfer_batch_size: 2_000,
            max_number_of_concurrent_sync_items: 4,
            skip_sync_preview_if_failed: true,
            max_percent_batch_fail: 0.5,
            max_batch_fail: 10,
            sync_conv_list_batch_max_attempts: 3,
            sync_paused_timeout_ms: 600_000,
            sync_conv_list_update_ui_batch_size: 1,
            manual_sync: SyncBranchConfig::default(),
            auto_sync: SyncBranchConfig::default(),
        }
    }
}

impl SyncRemoteConfig {
    /// Extracts `sync_2_pc` from a `getServerInfo` or `getLoginInfo` JSON response,
    /// falling back to Electron's bundled defaults for any missing or out-of-range key.
    #[must_use]
    pub fn from_server_info(value: &Value) -> Self {
        let mut cfg = Self::default();
        cfg.apply_server_info(value);
        cfg
    }

    /// Layers `features.sync_2_pc` from a `getServerInfo` or `getLoginInfo` JSON value
    /// onto this config, preserving existing values for any keys omitted in `value`.
    pub fn apply_server_info(&mut self, value: &Value) {
        if let Some(obj) = find_feature_object(value, "sync_2_pc") {
            self.apply_overlay(obj);
        }
    }

    /// Applies an overlay JSON object (`features.sync_2_pc`) onto this config.
    pub fn apply_overlay(&mut self, obj: &Value) {
        if let Some(b) = obj.get("enable").and_then(Value::as_bool) {
            self.enable = b;
        }
        if let Some(b) = obj.get("enable_on_sync_source_setting").and_then(Value::as_bool) {
            self.enable_on_sync_source_setting = b;
        }
        if let Some(n) = json_u64(obj.get("batch_size")).filter(|&n| (1..=10_000).contains(&n)) {
            self.batch_size = n;
        }
        if let Some(n) =
            json_u64(obj.get("conversations_batch_size")).filter(|&n| (1..=10_000).contains(&n))
        {
            self.conversations_batch_size = n;
        }
        if let Some(n) =
            json_u64(obj.get("transfer_batch_size")).filter(|&n| (50..=10_000).contains(&n))
        {
            self.transfer_batch_size = n;
        }
        if let Some(n) = json_u64(obj.get("max_number_of_concurrent_sync_items"))
            .filter(|&n| (1..=5).contains(&n))
        {
            self.max_number_of_concurrent_sync_items = n as usize;
        }
        if let Some(b) = obj.get("skip_sync_preview_if_failed").and_then(Value::as_bool) {
            self.skip_sync_preview_if_failed = b;
        }
        if let Some(f) = obj.get("max_percent_batch_fail").and_then(Value::as_f64) {
            if (0.0..=1.0).contains(&f) {
                self.max_percent_batch_fail = f;
            }
        }
        if let Some(n) = json_u64(obj.get("max_batch_fail")) {
            self.max_batch_fail = n as usize;
        }
        if let Some(n) = json_u64(obj.get("sync_conv_list_batch_max_attempts"))
            .filter(|&n| (1..=1_000).contains(&n))
        {
            self.sync_conv_list_batch_max_attempts = n as u32;
        }
        if let Some(n) = json_u64(obj.get("sync_paused_timeout_ms")).filter(|&n| n >= 1_000) {
            self.sync_paused_timeout_ms = n;
        }
        if let Some(n) = json_u64(obj.get("sync_conv_list_update_ui_batch_size")) {
            self.sync_conv_list_update_ui_batch_size = n as usize;
        }
        if let Some(m) = obj.get("manual_sync") {
            apply_branch(&mut self.manual_sync, m);
        }
        if let Some(a) = obj.get("auto_sync") {
            apply_branch(&mut self.auto_sync, a);
        }
    }

    /// Mirrors `SyncConfigService.isFeatureEnabledBySyncSource`.
    #[must_use]
    pub fn is_enabled_for(&self, source: SyncSource) -> bool {
        if !self.enable {
            return false;
        }
        match source {
            SyncSource::Manual => self.enable_on_sync_source_setting,
            SyncSource::AutoSync => self.auto_sync.enable,
        }
    }

    /// Returns the branch config for `source`.
    #[must_use]
    pub fn branch(&self, source: SyncSource) -> &SyncBranchConfig {
        match source {
            SyncSource::Manual => &self.manual_sync,
            SyncSource::AutoSync => &self.auto_sync,
        }
    }

    /// Mirrors `getTimeoutConfigs(source)`.
    #[must_use]
    pub fn timeout_configs(&self, source: SyncSource) -> TimeoutConfigs {
        let b = self.branch(source);
        TimeoutConfigs {
            confirm_timeout_ms: b.awaiting_mobile_timeout_ms,
            wakeup_timeout_ms: b.wakeup_timeout,
            batch_timeout_ms: b.batch_timeout,
        }
    }

    /// Computes a `[from_ts, to_ts]` threshold range from a `max_time_range_d` value
    /// (Electron `createThresholdRange`).
    #[must_use]
    pub fn threshold_range(max_days: i64, now_ms: u64) -> TimeRange {
        let from_ts = if max_days > 0 {
            let delta_ms = (max_days as u64).saturating_mul(86_400_000);
            now_ms.saturating_sub(delta_ms)
        } else {
            0
        };
        TimeRange {
            from_ts,
            to_ts: JS_MAX_SAFE_INTEGER,
        }
    }
}

fn apply_branch(dst: &mut SyncBranchConfig, obj: &Value) {
    if let Some(b) = obj.get("enable").and_then(Value::as_bool) {
        dst.enable = b;
    }
    if let Some(n) = json_u64(obj.get("awaiting_mobile_timeout_ms")).filter(|&n| n >= 1_000) {
        dst.awaiting_mobile_timeout_ms = n;
    }
    if let Some(n) = json_u64(obj.get("wakeup_timeout")) {
        dst.wakeup_timeout = n;
    }
    if let Some(n) = json_u64(obj.get("batch_timeout")) {
        dst.batch_timeout = n;
    }
    if let Some(s) = obj.get("sync_conv_list") {
        if let Some(v) = json_i64(s.get("max_time_range_d")) {
            dst.conv_list_max_time_range_d = v;
        }
        if let Some(v) = json_i64(s.get("max_number_of_conv")) {
            dst.conv_list_max_number_of_conv = v;
        }
    }
    if let Some(s) = obj.get("sync_last_chat") {
        if let Some(v) = json_i64(s.get("max_time_range_d")) {
            dst.last_chat_max_time_range_d = v;
        }
        if let Some(v) = json_i64(s.get("max_number_of_conv")) {
            dst.last_chat_max_number_of_conv = v;
        }
        if let Some(v) = json_i64(s.get("max_number_of_msg_per_conv")) {
            dst.last_chat_max_number_of_msg_per_conv = v;
        }
    }
    if let Some(s) = obj.get("sync_recent_chat") {
        if let Some(v) = json_i64(s.get("max_time_range_d")) {
            dst.recent_chat_max_time_range_d = v;
        }
        if let Some(v) = json_i64(s.get("max_number_of_conv")) {
            dst.recent_chat_max_number_of_conv = v;
        }
        if let Some(v) = json_i64(s.get("max_number_of_msg_per_conv")) {
            dst.recent_chat_max_number_of_msg_per_conv = v;
        }
    }
    if let Some(s) = obj.get("sync_last_and_recent_chat") {
        if let Some(v) = json_i64(s.get("max_time_range_d")) {
            dst.last_and_recent_max_time_range_d = v;
        }
    }
    if let Some(s) = obj.get("sync_all_remaining_msg") {
        if let Some(v) = json_i64(s.get("max_time_range_d")) {
            dst.remaining_max_time_range_d = v;
        }
        if let Some(v) = json_i64(s.get("max_number_of_conv")) {
            dst.remaining_max_number_of_conv = v;
        }
        if let Some(v) = json_i64(s.get("max_number_of_msg_per_conv")) {
            dst.remaining_max_number_of_msg_per_conv = v;
        }
        if let Some(v) = json_i64(s.get("enable_success_noti_close_time_ms")) {
            dst.close_success_timeout_ms = v;
        }
        if let Some(v) = json_i64(s.get("enable_failed_noti_close_time_ms")) {
            dst.close_error_timeout_ms = v;
        }
        if let Some(v) = json_i64(s.get("enable_fail_noti_close_time_ms")) {
            dst.close_fail_timeout_ms = v;
        }
        if let Some(v) = json_i64(s.get("enable_interrupt_noti_close_time_ms")) {
            dst.close_interrupt_timeout_ms = v;
        }
    }
}

/// Finds `features.<key>` inside a `getServerInfo` or `getLoginInfo` JSON value,
/// handling Zalo's `setttings` spelling as well as `settings`.
#[must_use]
pub fn find_feature_object<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    for container_key in ["setttings", "settings", "config"] {
        if let Some(obj) = root
            .get(container_key)
            .and_then(|s| s.get("features"))
            .and_then(|f| f.get(key))
            .filter(|v| v.is_object())
        {
            return Some(obj);
        }
    }
    root.get("features")
        .and_then(|f| f.get(key))
        .filter(|v| v.is_object())
        .or_else(|| root.get(key).filter(|v| v.is_object()))
}

fn json_u64(v: Option<&Value>) -> Option<u64> {
    let v = v?;
    v.as_u64()
        .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn json_i64(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}
