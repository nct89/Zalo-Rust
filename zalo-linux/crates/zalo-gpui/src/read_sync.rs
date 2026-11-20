//! Read receipts, typing state, and message reaction bookkeeping for `zalo-gpui`.

use std::collections::HashMap;

use serde_json::Value;
use zalo_client::{ReadMark, StatusUpdate};

/// One reaction attached to a local chat message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MsgReaction {
    /// Sender key (`"me"` for self, otherwise sender uid).
    pub sender_uid: String,
    /// Zalo reaction icon token (e.g. `"/-strong"`, `"/-heart"`).
    pub r_icon: String,
    /// Zalo numeric reaction type (`3` = like).
    pub r_type: i64,
}

/// Zalo PC hides the member list above this count (`max_member_group_show_seen`, default 20).
pub const MAX_GROUP_SEEN_MEMBERS: u32 = 20;

/// One member's seen and delivered cursors inside a group.
#[derive(Clone, Debug, Default)]
struct MemberCursor {
    /// Highest server id this member has seen.
    seen: i64,
    /// Highest server id delivered to this member.
    deliver: i64,
}

/// Syncs read state (`seenv2` outbound, `clearUnreads`/`seens` inbound) per thread.
#[derive(Clone, Debug, Default)]
pub struct ReadSync {
    /// Newest incoming message per `thread_id`: `(at_ms, seenv2 record)`.
    last_incoming: HashMap<String, (i64, Value)>,
    /// `gmi` of the newest message already acknowledged for `thread_id`.
    seen_sent: HashMap<String, String>,
    /// `gmi` already reported with `deliveredv2` for `thread_id`.
    delivered_sent: HashMap<String, String>,
    /// Highest server `msgId` known to have been read by the peer/group per `thread_id`.
    peer_seen: HashMap<String, i64>,
    /// Highest server `msgId` delivered but not necessarily seen, per `thread_id`.
    peer_delivered: HashMap<String, i64>,
    /// Group member cursors: `thread_id` → uid → cursor.
    members: HashMap<String, HashMap<String, MemberCursor>>,
}

impl ReadSync {
    /// Records an incoming message's `seenv2` payload when it is at least as new as
    /// what we already hold for `thread`.
    pub fn note_incoming(&mut self, thread: &str, at_ms: i64, record: &Value) {
        if gmi_of(record).is_none() || thread.is_empty() {
            return;
        }
        let newer = self
            .last_incoming
            .get(thread)
            .is_none_or(|(prev_at, _)| at_ms >= *prev_at);
        if newer {
            self.last_incoming
                .insert(thread.to_owned(), (at_ms, record.clone()));
        }
    }

    /// Returns `(gmi, record)` if `thread` has an incoming message not yet sent via `seenv2`.
    #[must_use]
    pub fn seen_due(&self, thread: &str) -> Option<(String, Value)> {
        let (_, record) = self.last_incoming.get(thread)?;
        let gmi = gmi_of(record)?;
        let already = self.seen_sent.get(thread).map(String::as_str) == Some(gmi.as_str());
        (!already).then(|| (gmi, record.clone()))
    }

    /// Marks `gmi` as already reported for `thread`.
    pub fn mark_seen_sent(&mut self, thread: &str, gmi: String) {
        self.seen_sent.insert(thread.to_owned(), gmi);
    }

    /// Returns `(gmi, record)` when `deliveredv2` has not been sent for the newest incoming message.
    #[must_use]
    pub fn delivered_due(&self, thread: &str) -> Option<(String, Value)> {
        let (_, record) = self.last_incoming.get(thread)?;
        let gmi = gmi_of(record)?;
        let already = self.delivered_sent.get(thread).map(String::as_str) == Some(gmi.as_str());
        (!already).then(|| (gmi, record.clone()))
    }

    /// Marks `gmi` as already reported via `deliveredv2`.
    pub fn mark_delivered_sent(&mut self, thread: &str, gmi: String) {
        self.delivered_sent.insert(thread.to_owned(), gmi);
    }

    /// Applies server status updates (`seens` / `groupSeens` / `delivereds` / `clearUnreads`).
    ///
    /// `self_uid` is this account. A `delivereds` row whose only uid is that
    /// account is ignored, matching Zalo PC. Returns `(thread_id, cutoff_at_ms)`
    /// pairs for threads whose unread state was cleared on another device.
    pub fn apply_status<F>(
        &mut self,
        status: &StatusUpdate,
        self_uid: Option<&str>,
        cutoff_for: F,
    ) -> Vec<(String, i64)>
    where
        F: Fn(&ReadMark) -> Option<i64>,
    {
        for mark in &status.seen {
            if mark.msg_id <= 0 || mark.thread_id.is_empty() {
                continue;
            }
            let entry = self.peer_seen.entry(mark.thread_id.clone()).or_insert(0);
            *entry = (*entry).max(mark.msg_id);
        }
        for mark in &status.delivered {
            if mark.msg_id <= 0 || mark.thread_id.is_empty() {
                continue;
            }
            let only_self = status.receipts.iter().any(|row| {
                row.thread_id == mark.thread_id
                    && row.msg_id == mark.msg_id
                    && !row.seen
                    && row.seen_uids.is_empty()
                    && only_self_uid(&row.delivered_uids, self_uid)
            });
            if only_self {
                continue;
            }
            let entry = self
                .peer_delivered
                .entry(mark.thread_id.clone())
                .or_insert(0);
            *entry = (*entry).max(mark.msg_id);
        }
        for row in &status.receipts {
            if row.msg_id <= 0 || row.thread_id.is_empty() {
                continue;
            }
            if !row.seen && row.seen_uids.is_empty() && only_self_uid(&row.delivered_uids, self_uid)
            {
                continue;
            }
            let map = self.members.entry(row.thread_id.clone()).or_default();
            for uid in &row.seen_uids {
                let cursor = map.entry(uid.clone()).or_default();
                cursor.seen = cursor.seen.max(row.msg_id);
            }
            for uid in &row.delivered_uids {
                if self_uid == Some(uid.as_str()) {
                    continue;
                }
                let cursor = map.entry(uid.clone()).or_default();
                cursor.deliver = cursor.deliver.max(row.msg_id);
            }
        }
        let mut cleared = Vec::new();
        for mark in &status.read {
            if mark.thread_id.is_empty() {
                continue;
            }
            let Some(cutoff) = cutoff_for(mark) else {
                continue;
            };
            if let Some((at_ms, record)) = self.last_incoming.get(&mark.thread_id) {
                if *at_ms <= cutoff {
                    if let Some(gmi) = gmi_of(record) {
                        self.seen_sent.insert(mark.thread_id.clone(), gmi);
                    }
                }
            }
            cleared.push((mark.thread_id.clone(), cutoff));
        }
        cleared
    }

    /// Returns `true` when the peer's read cursor reaches our message in `thread`.
    #[must_use]
    pub fn peer_saw<F>(
        &self,
        thread: &str,
        global_msg_id: Option<i64>,
        at_ms: i64,
        upto_at: F,
    ) -> bool
    where
        F: FnOnce(i64) -> Option<i64>,
    {
        covers(
            self.peer_seen.get(thread).copied(),
            global_msg_id,
            at_ms,
            upto_at,
        )
    }

    /// Returns `true` when a delivered cursor reaches our message.
    #[must_use]
    pub fn peer_got<F>(
        &self,
        thread: &str,
        global_msg_id: Option<i64>,
        at_ms: i64,
        upto_at: F,
    ) -> bool
    where
        F: FnOnce(i64) -> Option<i64>,
    {
        covers(
            self.peer_delivered.get(thread).copied(),
            global_msg_id,
            at_ms,
            upto_at,
        )
    }

    /// Members whose cursor covers `global_msg_id`. Seen uids are not repeated
    /// in the delivered list. Empty when the message has no server id.
    #[must_use]
    pub fn member_lists(
        &self,
        thread: &str,
        global_msg_id: Option<i64>,
    ) -> (Vec<String>, Vec<String>) {
        let Some(gid) = global_msg_id.filter(|&id| id > 0) else {
            return (Vec::new(), Vec::new());
        };
        let Some(map) = self.members.get(thread) else {
            return (Vec::new(), Vec::new());
        };
        let mut seen = Vec::new();
        let mut delivered = Vec::new();
        for (uid, cursor) in map {
            if cursor.seen >= gid {
                seen.push(uid.clone());
            } else if cursor.deliver >= gid {
                delivered.push(uid.clone());
            }
        }
        seen.sort();
        delivered.sort();
        (seen, delivered)
    }

    /// Cursors worth keeping across a restart. Incoming ack state stays in memory.
    #[must_use]
    pub fn cursors_json(&self) -> Value {
        let seen: serde_json::Map<String, Value> = self
            .peer_seen
            .iter()
            .filter(|(_, id)| **id > 0)
            .map(|(k, id)| (k.clone(), Value::from(*id)))
            .collect();
        let delivered: serde_json::Map<String, Value> = self
            .peer_delivered
            .iter()
            .filter(|(_, id)| **id > 0)
            .map(|(k, id)| (k.clone(), Value::from(*id)))
            .collect();
        let mut members = serde_json::Map::new();
        for (thread, map) in &self.members {
            let mut row = serde_json::Map::new();
            for (uid, cursor) in map {
                if cursor.seen <= 0 && cursor.deliver <= 0 {
                    continue;
                }
                row.insert(
                    uid.clone(),
                    serde_json::json!({"seen": cursor.seen, "deliver": cursor.deliver}),
                );
            }
            if !row.is_empty() {
                members.insert(thread.clone(), Value::Object(row));
            }
        }
        serde_json::json!({"seen": seen, "delivered": delivered, "members": members})
    }

    /// Restores cursors saved by [`Self::cursors_json`]. A later live event still wins.
    pub fn load_cursors(&mut self, value: &Value) {
        let num = |v: &Value| -> i64 {
            v.as_i64()
                .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
                .or_else(|| v.as_str()?.parse().ok())
                .unwrap_or(0)
        };
        if let Some(map) = value.get("seen").and_then(Value::as_object) {
            for (thread, id) in map {
                let id = num(id);
                if id > 0 {
                    let entry = self.peer_seen.entry(thread.clone()).or_insert(0);
                    *entry = (*entry).max(id);
                }
            }
        }
        if let Some(map) = value.get("delivered").and_then(Value::as_object) {
            for (thread, id) in map {
                let id = num(id);
                if id > 0 {
                    let entry = self.peer_delivered.entry(thread.clone()).or_insert(0);
                    *entry = (*entry).max(id);
                }
            }
        }
        if let Some(map) = value.get("members").and_then(Value::as_object) {
            for (thread, row) in map {
                let Some(people) = row.as_object() else {
                    continue;
                };
                let dest = self.members.entry(thread.clone()).or_default();
                for (uid, cursor) in people {
                    let seen = cursor.get("seen").map(num).unwrap_or(0);
                    let deliver = cursor.get("deliver").map(num).unwrap_or(0);
                    let slot = dest.entry(uid.clone()).or_default();
                    slot.seen = slot.seen.max(seen);
                    slot.deliver = slot.deliver.max(deliver);
                }
            }
        }
    }
}

fn only_self_uid(uids: &[String], self_uid: Option<&str>) -> bool {
    match (uids, self_uid) {
        ([only], Some(me)) => only == me,
        _ => false,
    }
}

fn covers<F>(cursor: Option<i64>, global_msg_id: Option<i64>, at_ms: i64, upto_at: F) -> bool
where
    F: FnOnce(i64) -> Option<i64>,
{
    let Some(cursor) = cursor.filter(|&id| id > 0) else {
        return false;
    };
    if let Some(gid) = global_msg_id.filter(|&id| id > 0) {
        return gid <= cursor;
    }
    upto_at(cursor).is_some_and(|seen_at| at_ms <= seen_at)
}

fn gmi_of(record: &Value) -> Option<String> {
    match record.get("gmi")? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Inserts or removes a sender's reaction on a message in the in-memory table.
pub fn update_reaction(
    by_msg: &mut HashMap<u64, Vec<MsgReaction>>,
    msg_id: u64,
    sender_uid: &str,
    r_icon: &str,
    r_type: i64,
    cleared: bool,
) {
    let list = by_msg.entry(msg_id).or_default();
    list.retain(|r| r.sender_uid != sender_uid);
    if !cleared {
        list.push(MsgReaction {
            sender_uid: sender_uid.to_owned(),
            r_icon: r_icon.to_owned(),
            r_type,
        });
    }
    if list.is_empty() {
        by_msg.remove(&msg_id);
    }
}

/// Official Zalo reaction buttons `(emoji, rIcon, rType, label_vi)` from `compact1847.js`.
pub const REACTION_PRESETS: &[(&str, &str, i64, &str)] = &[
    ("👍", "/-strong", 3, "Thích"),
    ("❤️", "/-heart", 5, "Yêu thích"),
    ("😆", ":>", 0, "Haha"),
    ("😮", ":o", 32, "Wow"),
    ("😢", ":-((", 2, "Buồn"),
    ("😠", ":-h", 20, "Phẫn nộ"),
];

/// Maps Zalo's reaction icon / type to a compact display emoji.
#[must_use]
pub fn reaction_emoji(r_icon: &str, r_type: i64) -> &'static str {
    match r_icon.trim() {
        "/-heart" => "❤️",
        "/-strong" => "👍",
        ":>" | ":)" => "😆",
        ":o" | ":-o" | ":-O" | ":O" => "😮",
        ":-(" | ":-((" | ":((" | ":(" => "😢",
        ":-h" | ":-H" | ":/" | ":-/" | ">:-(" => "😠",
        _ => match r_type {
            5 => "❤️",
            0 => "😆",
            32 | 1 => "😮",
            2 => "😢",
            20 | 4 => "😠",
            _ => "👍",
        },
    }
}

/// Normalizes thread ids returned by Zalo pin/archive APIs (`u123`, `g456`, `g_456`)
/// into `store.db` thread ids (`123`, `g_456`).
#[must_use]
pub fn normalize_thread_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s == "m1" {
        return None;
    }
    if let Some(rest) = s.strip_prefix("g_") {
        return (!rest.is_empty()).then(|| format!("g_{rest}"));
    }
    if let Some(rest) = s.strip_prefix('g') {
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            return Some(format!("g_{rest}"));
        }
    }
    if let Some(rest) = s.strip_prefix('u') {
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            return Some(rest.to_owned());
        }
    }
    Some(s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_incoming_and_seen_due_dedupe() {
        let mut sync = ReadSync::default();
        let rec1 = serde_json::json!({"gmi": "100", "cmi": "10"});
        let rec2 = serde_json::json!({"gmi": "200", "cmi": "20"});
        sync.note_incoming("42", 1_000, &rec1);
        sync.note_incoming("42", 900, &serde_json::json!({"gmi": "50"}));
        let (gmi, got) = sync.seen_due("42").expect("seen due");
        assert_eq!(gmi, "100");
        assert_eq!(got["cmi"], "10");

        sync.mark_seen_sent("42", gmi);
        assert!(sync.seen_due("42").is_none());

        sync.note_incoming("42", 2_000, &rec2);
        let (gmi2, _) = sync.seen_due("42").expect("newer message becomes due");
        assert_eq!(gmi2, "200");
    }

    #[test]
    fn delivered_due_does_not_consume_seen() {
        let mut sync = ReadSync::default();
        let rec = serde_json::json!({"gmi": "100", "cmi": "10"});
        sync.note_incoming("42", 1_000, &rec);
        let (gmi, _) = sync.delivered_due("42").expect("delivered");
        assert_eq!(gmi, "100");
        sync.mark_delivered_sent("42", gmi);
        assert!(sync.delivered_due("42").is_none());
        assert!(sync.seen_due("42").is_some());
    }

    #[test]
    fn apply_status_updates_peer_seen_and_prevents_stale_seenv2() {
        let mut sync = ReadSync::default();
        sync.note_incoming("77", 5_000, &serde_json::json!({"gmi": "500"}));
        let status = StatusUpdate::from_event(&serde_json::json!({
            "clearUnreads": [{"idTo": "77", "isGroup": 0, "lastMsgId": "500"}],
            "seens": [{"idTo": "77", "isGroup": 0, "msgId": "510"}]
        }));
        let cleared = sync.apply_status(&status, None, |_| Some(5_000));
        assert_eq!(cleared, vec![("77".to_owned(), 5_000)]);
        assert!(sync.seen_due("77").is_none());
        assert!(sync.peer_saw("77", Some(505), 4_000, |_| None));
        assert!(!sync.peer_saw("77", Some(520), 6_000, |_| None));
        assert!(sync.peer_saw("77", None, 4_500, |_| Some(5_000)));
    }

    #[test]
    fn delivered_cursor_and_group_member_lists() {
        let status = StatusUpdate::from_event(&serde_json::json!({
            "delivereds": [
                {"msgId": "30", "seen": 0, "threadId": "42"},
                {"msgId": "40", "seen": 0, "deliveredUids": ["me"], "threadId": "42"},
                {"msgId": "12", "seen": 0, "groupId": "9", "deliveredUids": ["me", "b"], "seenUids": ["a"]}
            ],
            "groupSeens": [{"groupId": "9", "msgId": 20, "seenUids": ["a", "0"]}]
        }));
        let mut sync = ReadSync::default();
        sync.apply_status(&status, Some("me"), |_| None);
        assert!(sync.peer_got("42", Some(30), 0, |_| None));
        assert!(!sync.peer_got("42", Some(40), 0, |_| None));
        assert!(sync.peer_saw("g_9", Some(20), 0, |_| None));
        let (seen, delivered) = sync.member_lists("g_9", Some(12));
        assert_eq!(seen, vec!["a".to_owned()]);
        assert_eq!(delivered, vec!["b".to_owned()]);
        let (seen_late, delivered_late) = sync.member_lists("g_9", Some(20));
        assert_eq!(seen_late, vec!["a".to_owned()]);
        assert!(delivered_late.is_empty());
        let saved = sync.cursors_json();
        assert!(saved.get("seen_sent").is_none());
        assert!(saved.get("last_incoming").is_none());
        let mut again = ReadSync::default();
        again.load_cursors(&saved);
        assert!(again.peer_got("42", Some(30), 0, |_| None));
        assert!(again.peer_saw("g_9", Some(20), 0, |_| None));
        let (seen_again, _) = again.member_lists("g_9", Some(12));
        assert_eq!(seen_again, vec!["a".to_owned()]);
    }

    #[test]
    fn normalize_thread_ids_and_reactions() {
        assert_eq!(normalize_thread_id("u12345").as_deref(), Some("12345"));
        assert_eq!(normalize_thread_id("g987").as_deref(), Some("g_987"));
        assert_eq!(normalize_thread_id("g_987").as_deref(), Some("g_987"));
        assert_eq!(normalize_thread_id("m1"), None);

        for &(emoji, r_icon, r_type, _) in REACTION_PRESETS {
            assert_eq!(reaction_emoji(r_icon, r_type), emoji);
            assert_eq!(reaction_emoji("", r_type), emoji);
        }

        let mut map = HashMap::new();
        update_reaction(&mut map, 1, "me", "/-heart", 5, false);
        assert_eq!(map[&1].len(), 1);
        assert_eq!(reaction_emoji(&map[&1][0].r_icon, map[&1][0].r_type), "❤️");
        update_reaction(&mut map, 1, "me", "", -1, true);
        assert!(map.is_empty());
    }
}
