//! Online dot and "last active" status.
//!
//! Zalo PC (`getLastActiveText`, `requestGetFriendOnlines`, `getOnlinStatus`):
//! a uid in `/api/social/friend/onlines` is online; the open 1-1 chat also
//! reads `/api/social/profile/lastOnline`. The list refreshes every 60s
//! (`list_friend_online_time`). A cached `lastOnline` is reused for 3 minutes
//! (`online_configs.user_active_time`). Groups are skipped (`enable_group`
//! defaults to off). The whole indicator hides when this account's
//! `show_online_status` is 0.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use gpui::Context;
use zalo_client::{
    LastOnline, AVATAR_MAX_BYTES, AVATAR_MIN_BYTES, AVATAR_MIN_EDGE, CONV_TYPE_USER,
};

use crate::data::ChatConv;
use crate::shell::ShellView;

/// `online_configs.user_active_time` (ms). Inside this window the subtitle
/// is "Just now" and the dot is on.
pub const USER_ACTIVE_MS: i64 = 180_000;
/// `list_friend_online_time` (ms).
pub const FRIEND_ONLINE_TTL_MS: i64 = 60_000;
/// Vietnam offset, same as the rest of the shell's clocks.
const VN_OFFSET_MS: i64 = 7 * 3_600_000;

/// Cached presence for the conversation list and the open chat.
#[derive(Clone, Debug, Default)]
pub struct Presence {
    /// Uids returned by the last `friend/onlines` call.
    pub online: HashSet<String>,
    /// Unix ms of that call. `0` means it has not run yet.
    pub list_at: i64,
    /// A list request is in flight.
    pub list_inflight: bool,
    /// `lastOnline` by thread id.
    pub last: HashMap<String, LastHit>,
    /// Threads whose `lastOnline` request is in flight.
    pub last_inflight: HashSet<String>,
    /// An avatar upload is in flight.
    pub avatar_busy: bool,
}

/// One `lastOnline` answer.
#[derive(Clone, Copy, Debug)]
pub struct LastHit {
    /// Unix ms. `0` when the server sent none.
    pub at_ms: i64,
    /// When this row was stored.
    pub fetched_at: i64,
    /// Peer `settings.show_online_status` is off.
    pub hidden: bool,
}

impl LastHit {
    fn from_reply(reply: LastOnline, now_ms: i64) -> Self {
        Self {
            at_ms: reply.at_ms,
            fetched_at: now_ms,
            hidden: reply.hidden,
        }
    }
}

/// Vietnamese size message from `update_avatar.min_size_kb` / `max_local_size`.
#[must_use]
pub fn avatar_size_error(len: u64) -> Option<&'static str> {
    if len < AVATAR_MIN_BYTES {
        Some("Ảnh đại diện quá nhỏ. Dung lượng tối thiểu là 7 KB.")
    } else if len > AVATAR_MAX_BYTES {
        Some("Ảnh đại diện quá lớn. Dung lượng tối đa là 30 MB.")
    } else {
        None
    }
}

/// Vietnamese edge message from `update_avatar.min_dimension_error`.
/// Unknown edges (`0`) are left for the server.
#[must_use]
pub fn avatar_edge_error(width: u32, height: u32) -> Option<&'static str> {
    if width == 0 || height == 0 {
        None
    } else if width < AVATAR_MIN_EDGE || height < AVATAR_MIN_EDGE {
        Some("Ảnh đại diện quá nhỏ. Kích thước tối thiểu là 240 x 240.")
    } else {
        None
    }
}

/// Maps a Zalo avatar error onto the PC toast. `166` is no permission,
/// `200` keeps the server message.
#[must_use]
pub fn avatar_fail_text(group: bool, err: &str) -> String {
    if err.contains("returned 166") {
        return "Bạn không đủ quyền đổi ảnh đại diện nhóm".to_owned();
    }
    if let Some(rest) = err.split("returned 200:").nth(1) {
        let msg = rest.trim();
        if !msg.is_empty() {
            return msg.to_owned();
        }
    }
    if group {
        "Có lỗi xảy ra khi đổi ảnh đại diện nhóm".to_owned()
    } else {
        "Đã có lỗi xảy ra. Vui lòng sử dụng ảnh khác hoặc thử lại sau".to_owned()
    }
}

/// `(text, just_now)` for a `lastOnline` timestamp.
/// `just_now` paints the green subtitle and the online dot.
#[must_use]
pub fn last_active_label(at_ms: i64, now_ms: i64) -> Option<(String, bool)> {
    const MIN_VALID_SECS: i64 = 946_684_800; // 2000-01-01T00:00:00Z
    const MAX_VALID_SECS: i64 = 4_102_444_800; // 2100-01-01T00:00:00Z
    const MIN_VALID_MS: i64 = MIN_VALID_SECS * 1000;
    if at_ms <= 0 || now_ms <= 0 {
        return None;
    }
    let at_ms = if (MIN_VALID_SECS..=MAX_VALID_SECS).contains(&at_ms) {
        at_ms * 1000
    } else {
        at_ms
    };
    if at_ms < MIN_VALID_MS {
        return None;
    }
    let age = now_ms - at_ms;
    if age <= -USER_ACTIVE_MS {
        return None;
    }
    if age <= USER_ACTIVE_MS {
        return Some(("Vừa truy cập".to_owned(), true));
    }
    let secs = at_ms / 1000;
    let now_secs = now_ms / 1000;
    if secs <= 0 {
        return None;
    }
    let delta = now_secs - secs;
    if delta < 3600 {
        return Some((format!("{} phút trước", delta / 60), false));
    }
    if delta < 86_400 {
        return Some((format!("{} giờ trước", delta / 3600), false));
    }
    let day = |ms: i64| (ms + VN_OFFSET_MS).div_euclid(86_400_000);
    if day(now_ms) - day(at_ms) == 1 {
        return Some(("Truy cập hôm qua".to_owned(), false));
    }
    let (y, m, d) = civil_from_unix_days(day(at_ms));
    Some((format!("{d}/{m}/{y}"), false))
}

/// Days since the Unix epoch (already shifted into Vietnam) → calendar date.
fn civil_from_unix_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    (y, m as u32, d as u32)
}

impl ShellView {
    /// This account still publishes / reads presence.
    /// Unknown settings stay on (the PC default of the toggle is 1).
    pub(crate) fn presence_visible(&self) -> bool {
        match self.privacy.values.as_ref() {
            Some(map) => map.get("show_online_status").copied().unwrap_or(1) != 0,
            None => true,
        }
    }

    /// Green dot for a friend uid.
    pub(crate) fn uid_online(&self, uid: &str) -> bool {
        if !self.presence_visible() || uid.is_empty() {
            return false;
        }
        if self.presence.last.get(uid).is_some_and(|hit| hit.hidden) {
            return false;
        }
        if self.presence.online.contains(uid) {
            return true;
        }
        self.presence.last.get(uid).is_some_and(|hit| {
            last_active_label(hit.at_ms, crate::data::now_ms()).is_some_and(|(_, fresh)| fresh)
        })
    }

    /// Green dot for a conversation row. Preview rows keep their fixture
    /// flag. Live groups stay off (`enable_group` is false).
    pub(crate) fn conv_online(&self, conv: &ChatConv) -> bool {
        if !self.presence_visible() {
            return false;
        }
        if conv.is_group {
            return false;
        }
        if self.uid_online(&conv.thread_id) {
            return true;
        }
        conv.online && !conv.live
    }

    /// Header subtitle under the name. Typing and the member count win.
    pub(crate) fn presence_subtitle(&self, conv: &ChatConv) -> Option<(String, bool)> {
        if !self.presence_visible() || conv.is_group {
            return None;
        }
        if self
            .presence
            .last
            .get(&conv.thread_id)
            .is_some_and(|hit| hit.hidden)
        {
            return None;
        }
        if self.presence.online.contains(&conv.thread_id) {
            return Some(("Vừa truy cập".to_owned(), true));
        }
        if let Some(hit) = self.presence.last.get(&conv.thread_id) {
            return last_active_label(hit.at_ms, crate::data::now_ms());
        }
        if conv.online && !conv.live {
            return Some(("Vừa truy cập".to_owned(), true));
        }
        None
    }

    fn last_online_due(&self, conv: &ChatConv, now_ms: i64) -> bool {
        if !self.presence_visible() || conv.is_group || !conv.live {
            return false;
        }
        if self.send2me_id.as_deref() == Some(conv.thread_id.as_str()) {
            return false;
        }
        if self.uin.as_deref() == Some(conv.thread_id.as_str()) {
            return false;
        }
        let friend = self
            .profiles
            .map
            .get(&conv.thread_id)
            .is_some_and(|p| p.friend);
        if !friend {
            return false;
        }
        match self.presence.last.get(&conv.thread_id) {
            Some(hit) => now_ms.saturating_sub(hit.fetched_at) > USER_ACTIVE_MS,
            None => true,
        }
    }

    /// One friend-online refresh (60s) and, for the open 1-1 chat, one
    /// `lastOnline` (3 min). Never runs from unit tests.
    pub(crate) fn drain_presence(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) || !self.has_session || !self.presence_visible() {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let now = crate::data::now_ms();
        if !self.presence.list_inflight
            && (self.presence.list_at == 0
                || now.saturating_sub(self.presence.list_at) > FRIEND_ONLINE_TTL_MS)
        {
            self.presence.list_inflight = true;
            let dir = dir.clone();
            cx.spawn(async move |this, cx| {
                let res = cx
                    .background_executor()
                    .spawn(async move {
                        crate::bridge::block_api(&dir, |api| async move {
                            api.friend_onlines().await.map_err(|e| e.to_string())
                        })
                    })
                    .await;
                let _ = this.update(cx, |view, cx| {
                    view.presence.list_inflight = false;
                    view.presence.list_at = crate::data::now_ms();
                    if let Ok(ids) = res {
                        view.presence.online = ids.into_iter().collect();
                        let online = view.presence.online.clone();
                        for person in &mut view.friends {
                            if !person.uid.is_empty() {
                                person.online = online.contains(&person.uid);
                            }
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        let Some(conv) = self.selected_conv() else {
            return;
        };
        if !self.last_online_due(&conv, now)
            || self.presence.last_inflight.contains(&conv.thread_id)
        {
            return;
        }
        let thread = conv.thread_id.clone();
        self.presence.last_inflight.insert(thread.clone());
        cx.spawn(async move |this, cx| {
            let thread_req = thread.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.last_online(&thread_req, CONV_TYPE_USER)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.presence.last_inflight.remove(&thread);
                let now = crate::data::now_ms();
                let hit = match res {
                    Ok(reply) => LastHit::from_reply(reply, now),
                    Err(_) => LastHit {
                        at_ms: 0,
                        fetched_at: now,
                        hidden: false,
                    },
                };
                view.presence.last.insert(thread, hit);
                cx.notify();
            });
        })
        .detach();
    }
}

/// Reads a picked picture and uploads it. `group` is the thread id (`g_…`)
/// or `None` for the signed-in account. Returns the https URL when the
/// server sent one (it may be empty; the PC refreshes the profile itself).
pub async fn upload_avatar_file(
    api: &zalo_client::ZaloApi,
    path: &Path,
    group: Option<&str>,
) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if let Some(msg) = avatar_size_error(bytes.len() as u64) {
        return Err(msg.to_owned());
    }
    let (width, height) = image::image_dimensions(path).unwrap_or((0, 0));
    if let Some(msg) = avatar_edge_error(width, height) {
        return Err(msg.to_owned());
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("avatar.jpg");
    let client_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1);
    let group_upload = group.is_some();
    let value = if let Some(thread) = group {
        api.upload_group_avatar(thread, bytes, name, client_id, width, height)
            .await
    } else {
        api.upload_profile_avatar(bytes, name, client_id, width, height)
            .await
    }
    .map_err(|e| avatar_fail_text(group_upload, &e.to_string()))?;
    Ok(zalo_client::avatar_url_from(&value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn just_now_then_minutes_hours_yesterday_and_a_date() {
        // 2023-11-15 05:13 Asia/Ho_Chi_Minh.
        let now = 1_700_000_000_000;
        let just = last_active_label(now - 60_000, now).unwrap();
        assert_eq!(just.0, "Vừa truy cập");
        assert!(just.1);
        assert!(last_active_label(now - 180_000, now).unwrap().1);
        assert_eq!(
            last_active_label(now - 181_000, now).unwrap().0,
            "3 phút trước"
        );
        assert_eq!(
            last_active_label(now - 5 * 3_600_000, now).unwrap().0,
            "5 giờ trước"
        );
        assert!(last_active_label(now + 200_000, now).is_none());
        assert!(last_active_label(0, now).is_none());
        assert!(last_active_label(13_910_400_000, now).is_none());
        // Seconds-scale timestamp normalizes to ms.
        assert_eq!(
            last_active_label((now - 181_000) / 1000, now).unwrap().0,
            "3 phút trước"
        );
        // 26h earlier is still the previous Vietnam calendar day.
        assert_eq!(
            last_active_label(now - 26 * 3_600_000, now).unwrap().0,
            "Truy cập hôm qua"
        );
        assert_eq!(
            last_active_label(now - 50 * 3_600_000, now).unwrap().0,
            "13/11/2023"
        );
    }

    #[test]
    fn avatar_limits_and_error_toasts() {
        assert!(avatar_size_error(100).is_some());
        assert!(avatar_size_error(8 * 1024).is_none());
        assert!(avatar_edge_error(0, 0).is_none());
        assert!(avatar_edge_error(100, 400).unwrap().contains("240"));
        assert!(avatar_fail_text(true, "Zalo returned 166: no").contains("quyền"));
        assert_eq!(
            avatar_fail_text(true, "Zalo returned 200: quá nặng"),
            "quá nặng"
        );
        assert!(avatar_fail_text(false, "timeout").contains("ảnh khác"));
    }
}
