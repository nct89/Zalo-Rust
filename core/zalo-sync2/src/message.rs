//! Turns `Sync2.Message.Message` records into messages the PC can store.
//!
//! Mirrors `syncMessage2SVMessage` in Zalo PC's `shared-worker.*.js`: ids are
//! converted from global ids to PC ids, the mobile `msg_type` becomes the PC
//! `msgType` string, and attachment-based messages take their content from
//! `attachs[0]`.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::proto::{Attach, Message, MessageChunk};
use crate::request::parse_partition;

/// PC `msgType` for a mobile `msg_type`, or `None` when PC skips the message.
pub fn pc_msg_type(msg_type: i32) -> Option<&'static str> {
    Some(match msg_type {
        0 => "webchat",
        2 => "chat.doodle",
        3 | 4 | 31 => "chat.photo",
        6 => "chat.voice",
        10 => "chat.sticker",
        12 => "chat.recommended",
        15 => "chat.list.action",
        18 => "chat.location.new",
        19 => "chat.video.msg",
        22 => "share.file",
        23 => "chat.gif",
        24 => "chat.webcontent",
        53 => "chat.webcontent.v2",
        25 => "chat.video.live.msg",
        33 => "chat.delete",
        _ => return None,
    })
}

/// Maps global ids (what the phone uses) to the ids this client uses.
///
/// Zalo PC fills this from the `globalId` field of friend profiles and group
/// info. Return `None` when the id is unknown; the message is then skipped.
pub trait IdMap {
    /// PC id of a user or group.
    fn pc_id(&self, global_id: &str, is_group: bool) -> Option<String>;
}

impl<F: Fn(&str, bool) -> Option<String>> IdMap for F {
    fn pc_id(&self, global_id: &str, is_group: bool) -> Option<String> {
        self(global_id, is_group)
    }
}

/// One message ready to store.
///
/// `thread_id`, `sender_id`, `cli_msg_id`, `global_msg_id`, `body` and
/// `sent_at` match `zalo_storage::MessageRecord`.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncedMessage {
    /// PC id of the conversation.
    pub thread_id: String,
    /// Whether the conversation is a group.
    pub is_group: bool,
    /// PC id of the sender.
    pub sender_id: String,
    /// Whether this account sent it.
    pub from_me: bool,
    /// `cliMsgId`.
    pub cli_msg_id: i64,
    /// `msgId`.
    pub global_msg_id: i64,
    /// PC `msgType`.
    pub msg_type: &'static str,
    /// Text for text messages; title or description for others.
    pub body: String,
    /// PC-shaped `content`: a string for text, an attachment object otherwise.
    pub content: Value,
    /// Unix milliseconds.
    pub sent_at: i64,
    /// TTL from the metadata, 0 when none.
    pub ttl: u64,
}

impl SyncedMessage {
    /// The message in the shape of a server message (`cmd 501/521` item), with
    /// `src` set to sync-from-mobile, so it can go through the normal path.
    pub fn to_server_json(&self) -> Value {
        json!({
            "msgId": self.global_msg_id.to_string(),
            "cliMsgId": self.cli_msg_id.to_string(),
            "msgType": self.msg_type,
            "uidFrom": if self.from_me { "0".to_owned() } else { self.sender_id.clone() },
            "idTo": self.thread_id,
            "content": self.content,
            "ts": self.sent_at.to_string(),
            "ttl": self.ttl,
            "notify": "1",
            "src": "sync_mobile_db",
        })
    }
}

fn attach_json(a: &Attach) -> Value {
    let mut m = Map::new();
    let mut put = |k: &str, v: &str| {
        m.insert(k.to_owned(), Value::String(v.to_owned()));
    };
    put("title", &a.title);
    put("description", &a.description);
    put("href", &a.href);
    put("thumb", &a.thumb);
    put("params", &a.params);
    put("action", &a.action);
    put("type", &a.kind);
    m.insert("childnumber".into(), json!(a.child_number));
    if a.cat_id != 0 || a.id != 0 {
        m.insert("catId".into(), json!(a.cat_id));
        m.insert("id".into(), json!(a.id));
    }
    Value::Object(m)
}

/// Converts one record. `self_id` is this account's PC id.
pub fn convert(
    message: &Message,
    thread_global_id: &str,
    is_group: bool,
    ids: &impl IdMap,
    self_id: &str,
) -> Option<SyncedMessage> {
    let msg_type = pc_msg_type(message.msg_type)?;
    let thread_id = ids.pc_id(thread_global_id, is_group)?;
    let sender_id = ids.pc_id(&message.sender_id, false)?;
    let first = message.meta.as_ref().and_then(|m| m.attachs.first());
    let (body, content) = match (message.msg_type, first) {
        (0, Some(a)) if a.action == "rtf" => {
            let mut c = attach_json(a);
            if a.title.is_empty() {
                c["title"] = Value::String(message.content.clone());
            }
            (message.content.clone(), c)
        }
        (0, _) => (
            message.content.clone(),
            Value::String(message.content.clone()),
        ),
        (24, a) if !a.is_some_and(|a| a.params.contains("pcItem")) => return None,
        (53, a) => {
            let a = a.cloned().unwrap_or_default();
            let c = json!({"zinstant_msg": a.zinstant_msg, "zinstant_data": a.zinstant_data, "action": a.action});
            (a.title, c)
        }
        (_, Some(a)) => {
            let body = if a.title.is_empty() {
                a.description.clone()
            } else {
                a.title.clone()
            };
            (body, attach_json(a))
        }
        (_, None) => (message.content.clone(), json!({})),
    };
    Some(SyncedMessage {
        from_me: sender_id == self_id,
        thread_id,
        is_group,
        sender_id,
        cli_msg_id: message.client_id as i64,
        global_msg_id: message.global_id,
        msg_type,
        body,
        content,
        sent_at: message.timestamp as i64,
        ttl: message.meta.as_ref().map_or(0, |m| m.ttl),
    })
}

/// Converts every message of a chunk, dropping ones already in `seen`
/// (by `global_msg_id`) and adding new ones to it.
pub fn convert_chunk(
    chunk: &MessageChunk,
    ids: &impl IdMap,
    self_id: &str,
    seen: &mut HashSet<i64>,
) -> Vec<SyncedMessage> {
    let mut out = Vec::new();
    for partition in &chunk.partitions {
        let Some((global_id, is_group)) = parse_partition(&partition.id) else {
            continue;
        };
        for message in &partition.messages {
            if seen.contains(&message.global_id) {
                continue;
            }
            if let Some(m) = convert(message, global_id, is_group, ids, self_id) {
                seen.insert(m.global_msg_id);
                out.push(m);
            }
        }
    }
    out
}
