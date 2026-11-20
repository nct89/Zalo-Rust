//! Read and seen state that Zalo attaches to message batches.
//!
//! Message events (`501/521`), the missed-message queue (`510/511`), status
//! pushes (`502/522`, `504/524`) and `get-last-msgs` may carry, next to the
//! messages:
//!
//! * `clearUnreads: [{idTo, isGroup, lastMsgId, ts, …}]` — this account read
//!   the conversation up to `lastMsgId` on another device (Zalo PC
//!   `gotClearUnreads`);
//! * `seens: [{idTo, isGroup, msgId, …}]` and
//!   `groupSeens: [{groupId, msgId, seenUids}]` — the other side saw our
//!   messages up to `msgId` (`processSeenFromServer`);
//! * `delivereds: [{msgId, seen, groupId | threadId | deliveredUids}]`.
//!
//! [`StatusUpdate::from_event`] collects those wherever they sit in the body.

use serde_json::Value;

/// "Read up to here on another device."
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadMark {
    /// User id, or `g_<groupId>`.
    pub thread_id: String,
    /// Newest read server message id, when given.
    pub last_msg_id: Option<i64>,
    /// Read time in ms, when given instead of an id.
    pub ts: Option<i64>,
}

/// "The other side saw our messages up to `msg_id`."
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenMark {
    /// User id, or `g_<groupId>`.
    pub thread_id: String,
    /// Server message id.
    pub msg_id: i64,
}

/// Member ids on one `groupSeens` or `delivereds` row.
///
/// Zalo PC (`getDeliveredsFromData`) keeps `seenUids` and `deliveredUids`
/// beside the message id. A row with `seen == 1` is also a seen cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberReceipt {
    /// User id, or `g_<groupId>`.
    pub thread_id: String,
    /// Server message id this receipt covers.
    pub msg_id: i64,
    /// `groupSeens`, or `delivereds` with `seen == 1`.
    pub seen: bool,
    /// Members who have seen the message. `"0"` is dropped.
    pub seen_uids: Vec<String>,
    /// Members who have received it. `"0"` is dropped.
    pub delivered_uids: Vec<String>,
}

/// Read and seen marks found in one event body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusUpdate {
    /// Conversations read elsewhere.
    pub read: Vec<ReadMark>,
    /// Our messages seen by the other side.
    pub seen: Vec<SeenMark>,
    /// Delivered but not yet seen (`delivereds` with `seen != 1`).
    pub delivered: Vec<SeenMark>,
    /// Per-row member lists from `groupSeens` and `delivereds`.
    pub receipts: Vec<MemberReceipt>,
}

fn id(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn is_group(item: &Value) -> bool {
    match item.get("isGroup") {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => s == "1" || s == "true",
        _ => false,
    }
}

fn uid_list(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let Some(uid) = text(Some(item)) else {
            continue;
        };
        if uid == "0" || out.contains(&uid) {
            continue;
        }
        out.push(uid);
    }
    out
}

fn thread(raw: &str, group: bool) -> String {
    let clean = raw.trim_start_matches("g_");
    if group {
        format!("g_{}", clean.trim_start_matches('g'))
    } else {
        clean.to_owned()
    }
}

impl StatusUpdate {
    /// Nothing was found.
    pub fn is_empty(&self) -> bool {
        self.read.is_empty()
            && self.seen.is_empty()
            && self.delivered.is_empty()
            && self.receipts.is_empty()
    }

    /// Collects marks from a decoded event body (any nesting under `data`).
    pub fn from_event(body: &Value) -> Self {
        let mut out = Self::default();
        out.collect(body, 0);
        out
    }

    fn collect(&mut self, value: &Value, depth: usize) {
        if depth > 4 {
            return;
        }
        let Some(map) = value.as_object() else {
            return;
        };
        for (key, child) in map {
            match (key.as_str(), child.as_array()) {
                ("clearUnreads", Some(items)) => {
                    for item in items {
                        let Some(raw) = text(item.get("idTo")) else {
                            continue;
                        };
                        let last_msg_id = id(item.get("lastMsgId")).filter(|n| *n > 0);
                        let ts = id(item.get("ts")).filter(|n| *n > 0);
                        if last_msg_id.is_none() && ts.is_none() {
                            continue;
                        }
                        self.read.push(ReadMark {
                            thread_id: thread(&raw, is_group(item)),
                            last_msg_id,
                            ts,
                        });
                    }
                }
                ("seens", Some(items)) => {
                    for item in items {
                        let (Some(raw), Some(msg)) = (
                            text(item.get("idTo")),
                            id(item.get("msgId")).or_else(|| id(item.get("realMsgId"))),
                        ) else {
                            continue;
                        };
                        self.seen.push(SeenMark {
                            thread_id: thread(&raw, is_group(item)),
                            msg_id: msg,
                        });
                    }
                }
                ("groupSeens", Some(items)) => {
                    for item in items {
                        let (Some(raw), Some(msg)) =
                            (text(item.get("groupId")), id(item.get("msgId")))
                        else {
                            continue;
                        };
                        let thread_id = thread(&raw, true);
                        self.seen.push(SeenMark {
                            thread_id: thread_id.clone(),
                            msg_id: msg,
                        });
                        self.receipts.push(MemberReceipt {
                            thread_id,
                            msg_id: msg,
                            seen: true,
                            seen_uids: uid_list(item.get("seenUids")),
                            delivered_uids: uid_list(item.get("deliveredUids")),
                        });
                    }
                }
                ("delivereds", Some(items)) => {
                    for item in items {
                        let seen = id(item.get("seen")).unwrap_or(0) == 1;
                        let Some(msg) = id(item.get("msgId")) else {
                            continue;
                        };
                        let delivered_uids = uid_list(item.get("deliveredUids"));
                        let seen_uids = uid_list(item.get("seenUids"));
                        // `groupId`, else `threadId`, else 1-1 `idTo` (same key as
                        // `seens`), else the first delivered uid.
                        let thread_id = text(item.get("groupId"))
                            .map(|g| thread(&g, true))
                            .or_else(|| text(item.get("threadId")))
                            .or_else(|| {
                                text(item.get("idTo")).map(|id| thread(&id, is_group(item)))
                            })
                            .or_else(|| delivered_uids.first().cloned());
                        let Some(thread_id) = thread_id else {
                            continue;
                        };
                        let mark = SeenMark {
                            thread_id: thread_id.clone(),
                            msg_id: msg,
                        };
                        if seen {
                            self.seen.push(mark);
                        } else {
                            self.delivered.push(mark);
                        }
                        self.receipts.push(MemberReceipt {
                            thread_id,
                            msg_id: msg,
                            seen,
                            seen_uids,
                            delivered_uids,
                        });
                    }
                }
                (_, _) if child.is_object() => self.collect(child, depth + 1),
                _ => {}
            }
        }
    }
}

/// Zalo's per-message record for `seenv2`: `{cmi, gmi, si, di, mt, st, at, cmd, ts}`
/// built from a raw message object as the socket delivered it.
pub fn seen_record(item: &Value, group: bool) -> Option<Value> {
    let cmi = text(item.get("cliMsgId"))?;
    let gmi = text(item.get("msgId"))?;
    let si = text(item.get("uidFrom"))?;
    let to = text(item.get("idTo")).unwrap_or_else(|| "0".to_owned());
    let to = if group {
        to.trim_start_matches("g_")
            .trim_start_matches('g')
            .to_owned()
    } else {
        to
    };
    let di = if group || to != si {
        to
    } else {
        "0".to_owned()
    };
    let num = |key: &str| id(item.get(key)).unwrap_or(-1);
    Some(serde_json::json!({
        "cmi": cmi,
        "gmi": gmi,
        "si": si,
        "di": di,
        "mt": text(item.get("msgType")).unwrap_or_else(|| "webchat".to_owned()),
        "st": num("st"),
        "at": num("at"),
        "cmd": num("cmd"),
        "ts": text(item.get("ts")).unwrap_or_else(|| "-1".to_owned()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_unreads_and_seens_are_found_under_data() {
        let body = serde_json::json!({"data": {
            "msgs": [],
            "clearUnreads": [
                {"idTo": "100000000000000001", "isGroup": 0, "lastMsgId": "8470721628411", "type": 1},
                {"idTo": "200000000000000002", "isGroup": 1, "lastMsgId": 0, "ts": 1794272107081_i64},
                {"idTo": "1", "isGroup": 0, "lastMsgId": 0}
            ],
            "seens": [{"idTo": "100000000000000001", "isGroup": 0, "msgId": "8471001164000"}],
            "groupSeens": [{"groupId": "200000000000000002", "msgId": 8471001164001_i64, "seenUids": ["1"]}],
            "delivereds": [{"msgId": "5", "seen": 1, "threadId": "42"}, {"msgId": "6", "seen": 0, "threadId": "42"}]
        }});
        let status = StatusUpdate::from_event(&body);
        assert_eq!(
            status.read,
            vec![
                ReadMark {
                    thread_id: "100000000000000001".into(),
                    last_msg_id: Some(8_470_721_628_411),
                    ts: None
                },
                ReadMark {
                    thread_id: "g_200000000000000002".into(),
                    last_msg_id: None,
                    ts: Some(1_794_272_107_081)
                },
            ]
        );
        assert_eq!(status.seen.len(), 3);
        assert!(status.seen.contains(&SeenMark {
            thread_id: "g_200000000000000002".into(),
            msg_id: 8_471_001_164_001
        }));
        assert!(status.seen.contains(&SeenMark {
            thread_id: "42".into(),
            msg_id: 5
        }));
        assert!(status.seen.contains(&SeenMark {
            thread_id: "100000000000000001".into(),
            msg_id: 8_471_001_164_000
        }));
        assert_eq!(
            status.delivered,
            vec![SeenMark {
                thread_id: "42".into(),
                msg_id: 6
            }]
        );
        let group = status
            .receipts
            .iter()
            .find(|r| r.thread_id == "g_200000000000000002")
            .expect("group receipt");
        assert!(group.seen);
        assert_eq!(group.seen_uids, vec!["1".to_owned()]);
        assert!(StatusUpdate::from_event(&serde_json::json!({"data": {"msgs": []}})).is_empty());
    }

    #[test]
    fn delivered_uids_fill_the_thread_when_ids_are_missing() {
        let body = serde_json::json!({
            "delivereds": [{
                "msgId": 9,
                "seen": 0,
                "deliveredUids": ["0", "77", "77"],
                "seenUids": ["88"]
            }]
        });
        let status = StatusUpdate::from_event(&body);
        assert_eq!(status.delivered[0].thread_id, "77");
        assert_eq!(status.receipts[0].delivered_uids, vec!["77".to_owned()]);
        assert_eq!(status.receipts[0].seen_uids, vec!["88".to_owned()]);
        assert!(!status.receipts[0].seen);
    }

    #[test]
    fn one_one_delivereds_use_id_to_like_seens() {
        let body = serde_json::json!({
            "delivereds": [{"msgId": "15", "seen": 0, "idTo": "42", "isGroup": 0}]
        });
        let status = StatusUpdate::from_event(&body);
        assert_eq!(status.delivered[0].thread_id, "42");
        assert_eq!(status.delivered[0].msg_id, 15);
        assert!(status.seen.is_empty());
    }

    #[test]
    fn seen_record_matches_zalo_pc() {
        let item = serde_json::json!({
            "msgId": "8469557464380", "cliMsgId": "1794220891640", "uidFrom": "100000000000000001",
            "idTo": "0", "msgType": "chat.recommended", "ts": "1794220891640", "st": 3, "at": 5, "cmd": 501
        });
        let rec = seen_record(&item, false).expect("record");
        assert_eq!(rec["di"], "0");
        assert_eq!(rec["si"], "100000000000000001");
        assert_eq!(rec["mt"], "chat.recommended");
        assert_eq!(rec["cmd"], 501);
        let group = serde_json::json!({
            "msgId": "1", "cliMsgId": "2", "uidFrom": "3", "idTo": "200000000000000002",
            "msgType": "webchat", "ts": "4"
        });
        let rec = seen_record(&group, true).expect("group");
        assert_eq!(rec["di"], "200000000000000002");
        assert_eq!(rec["st"], -1);
    }
}
