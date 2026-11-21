//! Exact port of Electron's `sync-full` state machine (`compact0909.js`),
//! conversation pagination macro (`compact3613.js`), and remaining messages
//! macro (`compact3272.js`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::config::{SyncRemoteConfig, SyncSource, TimeRange, SYNC_UNLIMITED_LIMIT};
use crate::proto::{SyncConversation, SyncMessage};
use crate::protocol::{
    format_partition, retry_backoff_ms, DisposeReason, MobileStatus, SyncDatabaseKind,
    SyncPriority,
};
use crate::session::{
    ConversationRemoteDb, MessageRemoteDb, SessionQuerySpec, SyncSessionError, SyncTransport,
};
use crate::wasm::SyncWasm;

/// One missing message range to synchronize for a conversation (`compact3272.js`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingMessageRange {
    /// Local PC conversation id (`<uid>` or `g_<grid>`).
    pub conv_id: String,
    /// Global noised conversation id used on the Sync 2 wire.
    pub global_noised_id: String,
    /// True if group chat (`"group/{globalId}"`), false if 1-1 (`"oneone/{globalId}"`).
    pub is_group: bool,
    /// Inclusive lower bound (ms).
    pub from_ts: u64,
    /// Inclusive upper bound (ms).
    pub to_ts: u64,
}

/// Reason a Sync 2 run terminated with failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncFailReason {
    /// Phone user tapped Reject (`status = 6`).
    MobileUserReject,
    /// Phone reported busy (`status = 8`).
    MobileBusy,
    /// Phone reported low storage (`status = 9`).
    MobileLowStorage,
    /// Phone logged out (`status = 10`).
    MobileLogout,
    /// Confirmation window expired (`awaiting_mobile_timeout_ms`, default 10 min).
    ConfirmTimeout,
    /// Exceeded `max_percent_batch_fail` (`compact3272.js`).
    ExceededMaxPercentBatchFail,
    /// Network / socket / decode error.
    Error(String),
}

/// Truthful progress snapshot emitted to the UI.
///
/// Matches Electron: percentage is ONLY present once at least one missing message
/// range is genuinely `isCompleted` (`floor(completed / total * 100)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncProgress {
    /// Waiting for user confirmation on mobile (`WaitingConfirm = 3`, or before confirm).
    WaitingConfirm,
    /// Phone is idle/paused (`Idle = 2`).
    MobileIdle,
    /// Syncing conversation list pages (`compact3613.js`).
    SyncingConversations {
        /// Number of conversations fetched so far.
        fetched: usize,
    },
    /// Preparing and pulling messages (`compact3272.js`).
    SyncingMessages {
        /// Completed missing ranges (`isCompleted == true`).
        completed_ranges: usize,
        /// Total missing ranges at the start of the remaining-messages phase.
        total_ranges: usize,
        /// `Some(floor(completed * 100 / total))` once at least one range has completed.
        percent: Option<u8>,
        /// Messages fetched so far.
        messages_fetched: usize,
    },
    /// Completed successfully.
    Success {
        /// Conversations synchronized.
        conversations: usize,
        /// Messages synchronized.
        messages: usize,
    },
    /// Cancelled by PC user or mobile user (`DisposeReason::UserCancel = 0`).
    Cancelled {
        /// True if cancelled from the mobile device (`status = 7`).
        by_mobile: bool,
    },
    /// Failed (`DisposeReason::Error = 2`).
    Failed(SyncFailReason),
}

/// Maps a terminal [`MobileStatus`] to either a cancel (`DisposeReason::UserCancel = 0`)
/// or failure (`DisposeReason::Error = 2`), per `compact0909.js`.
#[must_use]
pub fn classify_mobile_status(status: MobileStatus) -> MobileStatusDisposition {
    match status {
        MobileStatus::Active => MobileStatusDisposition::UiActive,
        MobileStatus::Idle => MobileStatusDisposition::UiIdle,
        MobileStatus::WaitingConfirm => MobileStatusDisposition::UiWaitingConfirm,
        MobileStatus::Confirmed | MobileStatus::BypassConfirmed => {
            MobileStatusDisposition::UiConfirmed
        }
        MobileStatus::UserCancel => MobileStatusDisposition::Cancel(DisposeReason::UserCancel),
        MobileStatus::UserReject => {
            MobileStatusDisposition::Fail(SyncFailReason::MobileUserReject, DisposeReason::Error)
        }
        MobileStatus::MasterDeviceBusy => {
            MobileStatusDisposition::Fail(SyncFailReason::MobileBusy, DisposeReason::Error)
        }
        MobileStatus::MasterDeviceLowStorage => {
            MobileStatusDisposition::Fail(SyncFailReason::MobileLowStorage, DisposeReason::Error)
        }
        MobileStatus::MasterDeviceLogout => {
            MobileStatusDisposition::Fail(SyncFailReason::MobileLogout, DisposeReason::Error)
        }
    }
}

/// Action to take on the `sync-full` state machine when a `transfer_status` arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MobileStatusDisposition {
    /// Update UI to active / syncing (no extra 590).
    UiActive,
    /// Update UI to paused / idle (no extra 590).
    UiIdle,
    /// Update UI to waiting for phone confirmation (no extra 590).
    UiWaitingConfirm,
    /// Phone confirmed / bypassed confirmation (no extra 590).
    UiConfirmed,
    /// Cancel sync and dispose with `DisposeReason::UserCancel` (0).
    Cancel(DisposeReason),
    /// Fail sync and dispose with `DisposeReason::Error` (2).
    Fail(SyncFailReason, DisposeReason),
}

/// Result of [`sync_conversations_macro`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationSyncOutput {
    /// All conversation records fetched across all pages.
    pub conversations: Vec<SyncConversation>,
    /// Number of pages executed.
    pub pages: usize,
}

/// Exact port of `compact3613.js` conversation pagination loop:
///
/// - Keeps `ConversationRemoteDb` open across ALL pages (NEVER sends `591` between pages).
/// - Runs `prepare` once before pagination.
/// - Page 1 uses `last_chat.max_number_of_conv` (fallback `10`) when positive, else `conversations_batch_size` (fallback `1000`).
/// - Subsequent pages use `conversations_batch_size` (fallback `1000`).
/// - `has_more` is `page.len() as u64 == n` (exact equality).
/// - Next page sets `to_ts = last.last_ts` (NO `-1`!).
/// - Empty page breaks pagination.
/// - Retries retryable errors up to `sync_conv_list_batch_max_attempts` (fallback 3) with `min(1000*2^(a-1), 3000)` ms.
/// - On success ONLY, closes the conversation remote DB ONCE with `DisposeReason::SyncSuccess` (`1`).
#[allow(clippy::too_many_arguments)]
pub async fn sync_conversations_macro<T, F>(
    db: &ConversationRemoteDb,
    transport: &T,
    wasm: &Mutex<SyncWasm>,
    config: &SyncRemoteConfig,
    source: SyncSource,
    threshold_range: TimeRange,
    last_sync_conv_success_ts: u64,
    cancelled: &AtomicBool,
    mut on_page: F,
) -> Result<ConversationSyncOutput, SyncSessionError>
where
    T: SyncTransport,
    F: FnMut(&[SyncConversation], usize),
{
    let branch = config.branch(source);
    let cap = branch.conv_list_max_number_of_conv;
    let unlimited = cap < 0;
    if cap == 0 {
        db.close(transport, DisposeReason::SyncSuccess).await;
        return Ok(ConversationSyncOutput {
            conversations: Vec::new(),
            pages: 0,
        });
    }

    // Prepare conversation query once on the open DB (`prepareSyncConversations`).
    db.prepare(
        threshold_range,
        cap,
        SyncPriority::High,
        transport,
        wasm,
    )
    .await?;

    let client_batch_size = config.conversations_batch_size.max(1);
    let preview_first_page = branch.last_chat_max_number_of_conv;
    let max_attempts = config.sync_conv_list_batch_max_attempts.max(1);

    let from_ts = last_sync_conv_success_ts.max(threshold_range.from_ts);
    let mut to_ts = threshold_range.to_ts;
    let mut has_more = true;
    let mut is_first_page = true;
    let mut fetched: u64 = 0;
    let mut all_convs: Vec<SyncConversation> = Vec::new();
    let mut pages = 0usize;

    while has_more && (unlimited || fetched < cap as u64) {
        if cancelled.load(Ordering::Relaxed) {
            return Err(SyncSessionError::Released);
        }
        let mut n = client_batch_size;
        if is_first_page && preview_first_page > 0 {
            n = preview_first_page as u64;
        }
        if !unlimited {
            n = n.min((cap as u64).saturating_sub(fetched));
        }
        if n == 0 {
            break;
        }

        let page_range = TimeRange { from_ts, to_ts };
        let mut attempt = 1u32;
        let rows = loop {
            match db
                .execute_get_all(page_range, n, SyncPriority::High, transport, wasm)
                .await
            {
                Ok(rows) => break rows,
                Err(err) => {
                    let retryable = !matches!(
                        err,
                        SyncSessionError::Released
                            | SyncSessionError::Rejected211
                            | SyncSessionError::ConnectTimeout
                    );
                    if attempt >= max_attempts || !retryable || cancelled.load(Ordering::Relaxed) {
                        return Err(err);
                    }
                    tokio::time::sleep(Duration::from_millis(retry_backoff_ms(attempt))).await;
                    attempt += 1;
                }
            }
        };

        pages += 1;
        let page_has_more = rows.len() as u64 == n;
        fetched += rows.len() as u64;
        on_page(&rows, fetched as usize);

        if let Some(last) = rows.last() {
            // Electron `compact3613.js:347`: `g = a[a.length - 1].lastTs` (NO -1!).
            to_ts = last.last_ts;
            has_more = page_has_more;
        } else {
            // Empty page breaks pagination even if `has_more` was true.
            has_more = false;
        }
        all_convs.extend(rows);
        is_first_page = false;
    }

    // Close conversation remote database ONCE with SyncSuccess (1) on success.
    db.close(transport, DisposeReason::SyncSuccess).await;
    Ok(ConversationSyncOutput {
        conversations: all_convs,
        pages,
    })
}

/// Summary of [`sync_other_messages_macro`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagesSyncOutput {
    /// Total ranges at start (`totalCount`).
    pub total_ranges: usize,
    /// Ranges that genuinely completed (`isCompleted == true`).
    pub completed_ranges: usize,
    /// Total messages fetched across all ranges.
    pub messages_fetched: usize,
    /// Successful individual batch calls.
    pub successful_batches: usize,
    /// Failed individual batch calls.
    pub failed_batches: usize,
}

/// Computes Electron's exact batch fail ratio check (`compact3272.js` `checkBatchFailPercentage`):
/// returns `true` if `failed / (failed + successful) > max_percent_batch_fail` (denominator 0 -> false).
#[must_use]
pub fn exceeded_max_percent_batch_fail(
    failed: usize,
    successful: usize,
    max_percent_batch_fail: f64,
) -> bool {
    let total = failed + successful;
    if total == 0 {
        return false;
    }
    (failed as f64) / (total as f64) > max_percent_batch_fail
}

/// Computes Electron's exact progress percentage (`floor(completed * 100 / total)`).
#[must_use]
pub fn compute_completion_percent(completed: usize, total: usize) -> Option<u8> {
    if total == 0 || completed == 0 {
        return None;
    }
    let pct = ((completed.min(total) as u64) * 100 / (total as u64)).min(100);
    Some(pct as u8)
}

/// Exact port of `compact3272.js` remaining-messages phase:
///
/// 1. `prepare_messages`: every missing range gets `limit = 2147483647` (`SYNC_UNLIMITED_LIMIT`),
///    `priority = Low (2)`, grouped `< 30` queries per `590` ACK without releasing prior sessions.
/// 2. `syncAllMessages`: runs up to `max_number_of_concurrent_sync_items` (fallback `4`) concurrent
///    per-conversation `GetAll` tasks, each with `limit = config.batch_size` (fallback `2000`),
///    `priority = Low (2)`.
///    - `has_more = rows.len() as u64 >= limit`.
///    - 0 rows → `isCompleted = true`, `hasMore = false`.
///    - `has_more` with rows → `range.to_ts = last.timestamp` (NO `-1`!), `isCompleted = false`, loop.
///    - `!has_more` → `isCompleted = true` → increment `completed_count`, report `% = floor(completed/total*100)`.
///    - Failed batch: retry once (`retry_count < 1`).
/// 3. At end, checks `checkBatchFailPercentage` against `max_percent_batch_fail`.
#[allow(clippy::too_many_arguments)]
pub async fn sync_other_messages_macro<T, FMsg, FProg>(
    db: Arc<MessageRemoteDb>,
    transport: Arc<T>,
    wasm: Arc<Mutex<SyncWasm>>,
    config: &SyncRemoteConfig,
    ranges: Vec<MissingMessageRange>,
    cancelled: Arc<AtomicBool>,
    on_messages: Arc<FMsg>,
    on_progress: Arc<FProg>,
) -> Result<MessagesSyncOutput, SyncFailReason>
where
    T: SyncTransport + 'static,
    FMsg: Fn(&MissingMessageRange, Vec<SyncMessage>) + Send + Sync + 'static,
    FProg: Fn(usize, usize, Option<u8>, usize) + Send + Sync + 'static,
{
    let total_ranges = ranges.len();
    if total_ranges == 0 {
        return Ok(MessagesSyncOutput {
            total_ranges: 0,
            completed_ranges: 0,
            messages_fetched: 0,
            successful_batches: 0,
            failed_batches: 0,
        });
    }

    // Step 1: PrepareAll (`prepareSyncOtherMessages`, limit = 2147483647, priority = Low(2)).
    let prepare_specs: Vec<SessionQuerySpec> = ranges
        .iter()
        .filter(|r| !r.global_noised_id.is_empty())
        .map(|r| SessionQuerySpec {
            partition: format_partition(&r.global_noised_id, r.is_group),
            from: r.from_ts,
            to: r.to_ts,
            limit: SYNC_UNLIMITED_LIMIT,
            db_kind: SyncDatabaseKind::Message,
            priority: SyncPriority::Low,
        })
        .collect();

    if !prepare_specs.is_empty() {
        db.prepare_messages(prepare_specs, transport.as_ref(), wasm.as_ref())
            .await
            .map_err(|e| match e {
                SyncSessionError::ConnectTimeout => SyncFailReason::ConfirmTimeout,
                other => SyncFailReason::Error(other.to_string()),
            })?;
    }

    // Step 2: Concurrent per-conversation GetAll (`fillProcessingSlots`, default 4 slots).
    let concurrency = config.max_number_of_concurrent_sync_items.clamp(1, 5);
    let batch_size = config.batch_size.max(1);
    let sem = Arc::new(Semaphore::new(concurrency));

    let completed_ranges = Arc::new(AtomicUsize::new(0));
    let messages_fetched = Arc::new(AtomicUsize::new(0));
    let successful_batches = Arc::new(AtomicUsize::new(0));
    let failed_batches = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(total_ranges);
    for range in ranges {
        let sem = Arc::clone(&sem);
        let db = Arc::clone(&db);
        let transport = Arc::clone(&transport);
        let wasm = Arc::clone(&wasm);
        let cancelled = Arc::clone(&cancelled);
        let completed_ranges = Arc::clone(&completed_ranges);
        let messages_fetched = Arc::clone(&messages_fetched);
        let successful_batches = Arc::clone(&successful_batches);
        let failed_batches = Arc::clone(&failed_batches);
        let on_messages = Arc::clone(&on_messages);
        let on_progress = Arc::clone(&on_progress);

        handles.push(tokio::spawn(async move {
            let Ok(_permit) = sem.acquire_owned().await else {
                return;
            };
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            if range.global_noised_id.is_empty() {
                failed_batches.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let partition = format_partition(&range.global_noised_id, range.is_group);
            let mut cur = range;
            let mut retries = 0u8;

            loop {
                if cancelled.load(Ordering::Relaxed) {
                    return;
                }
                let q_range = TimeRange {
                    from_ts: cur.from_ts,
                    to_ts: cur.to_ts,
                };
                match db
                    .execute_get_all(
                        &partition,
                        q_range,
                        batch_size,
                        SyncPriority::Low,
                        transport.as_ref(),
                        wasm.as_ref(),
                    )
                    .await
                {
                    Ok(rows) => {
                        successful_batches.fetch_add(1, Ordering::Relaxed);
                        let row_count = rows.len();
                        let has_more = (row_count as u64) >= batch_size;
                        let last_ts = rows.last().map(|m| m.timestamp).unwrap_or(0);
                        if row_count > 0 {
                            let total_msgs =
                                messages_fetched.fetch_add(row_count, Ordering::Relaxed)
                                    + row_count;
                            on_messages(&cur, rows);
                            if has_more && last_ts > 0 && last_ts < cur.to_ts {
                                // Electron `compact3272.js:99`: `toTs = lastTs` (NO -1!), `isCompleted = false`.
                                cur.to_ts = last_ts;
                                let done = completed_ranges.load(Ordering::Relaxed);
                                on_progress(
                                    done,
                                    total_ranges,
                                    compute_completion_percent(done, total_ranges),
                                    total_msgs,
                                );
                                continue;
                            }
                        }
                        // Range is genuinely `isCompleted == true` -> increment & report %.
                        let done = completed_ranges.fetch_add(1, Ordering::Relaxed) + 1;
                        let total_msgs = messages_fetched.load(Ordering::Relaxed);
                        on_progress(
                            done,
                            total_ranges,
                            compute_completion_percent(done, total_ranges),
                            total_msgs,
                        );
                        return;
                    }
                    Err(_) => {
                        failed_batches.fetch_add(1, Ordering::Relaxed);
                        if retries < 1 {
                            retries += 1;
                            continue;
                        }
                        return;
                    }
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let ok_batches = successful_batches.load(Ordering::Relaxed);
    let fail_batches = failed_batches.load(Ordering::Relaxed);
    if exceeded_max_percent_batch_fail(fail_batches, ok_batches, config.max_percent_batch_fail) {
        return Err(SyncFailReason::ExceededMaxPercentBatchFail);
    }

    Ok(MessagesSyncOutput {
        total_ranges,
        completed_ranges: completed_ranges.load(Ordering::Relaxed),
        messages_fetched: messages_fetched.load(Ordering::Relaxed),
        successful_batches: ok_batches,
        failed_batches: fail_batches,
    })
}
