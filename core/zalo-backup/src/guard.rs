//! Backup UI state enums, spam rule tracker, cooldown check, and confirm-new-backup guard
//! faithfully ported from `compact3197.js` and `main-startup`.

use serde::{Deserialize, Serialize};

use crate::config::BackupRemoteConfig;
use crate::protocol::ExistingBackupInfo;

/// Sidebar banner states (`compact3197.js` enum `a`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BackupBannerState {
    #[default]
    Hidden,
    WaitForNetwork,
    InProgress {
        percent: u32,
    },
    Success {
        file_size: u64,
    },
    AfterSuccess {
        file_size: u64,
    },
    Fail {
        category: BackupErrorCategory,
        can_retry: bool,
        message: String,
    },
    Interrupted {
        attempt: u32,
        max_attempts: u32,
    },
}

/// Popup states (`compact3197.js` enum `i`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BackupPopupState {
    #[default]
    None,
    /// Shown only after 1000ms delay while `634`/`632` are in flight.
    Loading,
    /// No existing backup (or local TTL expired).
    Intro,
    /// Existing backup metadata found and not locally expired.
    ExistingBackup(ExistingBackupInfo),
    /// Offline / network error before starting backup.
    NetworkError,
    /// Generic error (e.g., empty `filePath` or unexpected failure).
    GenericError { message: String },
    /// Spam-locked popup (shown when user clicks "Create new" from `Intro` while locked).
    SpamLocked { unlock_at_ms: u64 },
}

/// Banner error categories (`compact3197.js` enum `E`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupErrorCategory {
    Network,
    UserSystem,
    FullStorage,
    InterruptByDevice,
    Other,
}

/// Which popup the user clicked "Create new backup" from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmSourcePopup {
    Intro,
    ExistingBackup,
}

/// Spam rule tracker (`z_backup_sr_` in Electron).
///
/// Exact Electron behavior:
/// - `record_request(now)` keeps timestamps within `[now - fixed_time*1000, now]`,
///   pushes `now`, and if `timestamps.len() > max_request` (e.g. 7th request when `max_request == 6`),
///   CLEARS the list and locks until `now + restrict_time * 1000`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpamRuleTracker {
    pub request_timestamps_ms: Vec<u64>,
    pub locked_until_ms: Option<u64>,
}

impl SpamRuleTracker {
    /// Returns remaining lock time in ms if currently locked at `now_ms`.
    #[must_use]
    pub fn remaining_lock_ms(&self, now_ms: u64) -> Option<u64> {
        let until = self.locked_until_ms?;
        if now_ms < until {
            Some(until - now_ms)
        } else {
            None
        }
    }

    /// Returns `true` if spam lock is active at `now_ms`.
    #[must_use]
    pub fn is_locked(&self, now_ms: u64) -> bool {
        self.remaining_lock_ms(now_ms).is_some()
    }

    /// Records a new create-backup request at `now_ms`.
    /// Returns `Some(unlock_at_ms)` if this request tripped the spam limit (exceeded `max_request`),
    /// clearing the list and locking for `restrict_time` seconds.
    pub fn record_request(&mut self, now_ms: u64, cfg: &BackupRemoteConfig) -> Option<u64> {
        let window_ms = cfg.fixed_time.saturating_mul(1_000);
        let cutoff = now_ms.saturating_sub(window_ms);
        self.request_timestamps_ms.retain(|&ts| ts >= cutoff);
        self.request_timestamps_ms.push(now_ms);
        if self.request_timestamps_ms.len() > cfg.max_request as usize {
            self.request_timestamps_ms.clear();
            let unlock_at = now_ms.saturating_add(cfg.restrict_time.saturating_mul(1_000));
            self.locked_until_ms = Some(unlock_at);
            return Some(unlock_at);
        }
        None
    }
}

/// Checks whether post-success cooldown (`create_backup_waiting_time` seconds, default 600s) is active.
/// Returns `Some(remaining_ms)` when cooldown blocks a new backup.
#[must_use]
pub fn check_cooldown(
    last_success_at_ms: Option<u64>,
    now_ms: u64,
    cfg: &BackupRemoteConfig,
) -> Option<u64> {
    let waiting_ms = cfg.cooldown_ms();
    if waiting_ms == 0 {
        return None;
    }
    let last = last_success_at_ms?;
    let elapsed = now_ms.saturating_sub(last);
    if elapsed < waiting_ms {
        Some(waiting_ms - elapsed)
    } else {
        None
    }
}

/// Outcome of the 5-step `onUserConfirmNewBackup` guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmNewBackupDecision {
    /// Step 1: A backup is already running — ignore (QoS concurrent). No `recordRequest`.
    IgnoreConcurrent,
    /// Step 2: Cooldown active — show cooldown toast. No `recordRequest`.
    CooldownToast { remaining_ms: u64 },
    /// Step 3a: Spam locked from `ExistingBackup` popup — show spam toast. No `recordRequest`.
    SpamToast { remaining_ms: u64 },
    /// Step 3b: Spam locked from `Intro` popup — switch to `SpamLocked` popup. No `recordRequest`.
    SpamPopup { unlock_at_ms: u64 },
    /// Step 4: Another heavy task (Sync 2 or Export/Import) is running — ignore. No `recordRequest`.
    ExternalBusy,
    /// Step 5a: Passed guards, `recordRequest` executed, but device is offline → show `NetworkError` popup.
    OfflineNetworkError,
    /// Step 5b: Passed all guards, `recordRequest` executed, online → hide popup and start fresh backup!
    StartFreshBackup,
}

/// Evaluates the exact 5-step `onUserConfirmNewBackup` guard order from Electron:
/// 1. Running → stop (no `recordRequest`).
/// 2. Cooldown → toast (no `recordRequest`).
/// 3. Spam locked → toast if `ExistingBackup`, popup if `Intro` (no `recordRequest`).
/// 4. `is_external_busy` → stop (no `recordRequest`).
/// 5. `record_request(now_ms)` (note: if the 7th request trips lock right now, Electron has already
///    passed `enforceSpamLimit` so it proceeds if not previously locked — wait: in Electron
///    `enforceSpamLimit` checks `isLocked` BEFORE `recordRequest`, and `recordRequest` arms the lock
///    for subsequent attempts!), then check network (`is_online`), then start backup.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_confirm_new_backup(
    is_running: bool,
    is_external_busy: bool,
    is_online: bool,
    source_popup: ConfirmSourcePopup,
    last_success_at_ms: Option<u64>,
    now_ms: u64,
    cfg: &BackupRemoteConfig,
    spam: &mut SpamRuleTracker,
) -> ConfirmNewBackupDecision {
    // 1. Running
    if is_running {
        return ConfirmNewBackupDecision::IgnoreConcurrent;
    }

    // 2. Cooldown (does NOT call recordRequest)
    if let Some(remaining_ms) = check_cooldown(last_success_at_ms, now_ms, cfg) {
        return ConfirmNewBackupDecision::CooldownToast { remaining_ms };
    }

    // 3. Spam limit check (does NOT call recordRequest)
    if let Some(remaining_ms) = spam.remaining_lock_ms(now_ms) {
        let unlock_at_ms = spam.locked_until_ms.unwrap_or(now_ms + remaining_ms);
        return match source_popup {
            ConfirmSourcePopup::ExistingBackup => ConfirmNewBackupDecision::SpamToast { remaining_ms },
            ConfirmSourcePopup::Intro => ConfirmNewBackupDecision::SpamPopup { unlock_at_ms },
        };
    }

    // 4. External busy (does NOT call recordRequest)
    if is_external_busy {
        return ConfirmNewBackupDecision::ExternalBusy;
    }

    // 5. Record request, then check network, then start
    let _ = spam.record_request(now_ms, cfg);
    if !is_online {
        return ConfirmNewBackupDecision::OfflineNetworkError;
    }
    ConfirmNewBackupDecision::StartFreshBackup
}
