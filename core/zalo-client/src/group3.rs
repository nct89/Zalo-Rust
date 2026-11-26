//! Group 3 (P1) Zalo PC APIs:
//! - Server-synced conversation label management 26.11.12 (`/api/convlabel/manage/*`)
//! - Conversation block-sharing 26.11.12 (`/api/conv/blocksharing/*`)
//! - Advanced group management (`blockedmems`, `link/ginfo|join`, `invite|kickout/multi`,
//!   `inv-box/inv-info`, `analyse`, `topic/remove`, `quota/members-comm`)
//! - Group multi-pin, reminders, 1-on-1 `friendboard` pins, `/api/poll/share`,
//!   and Zalo To-Do (`/api/board/personal/*`, 9 APIs).

use std::collections::HashSet;

use serde_json::{json, Value};

use super::api::{ClientError, ZaloApi};
use super::ops::strip_group;

/// Default avatar size requested on group link/invitation endpoints (`compact3178.js` `oe = 120`).
const AVATAR_SIZE: i64 = 120;

/// Status constant for an open Zalo To-Do task (`IN_PROCESS`, `compact0987.js:57`).
pub const TODO_STATUS_OPEN: i64 = 0;
/// Status constant for a completed Zalo To-Do task (`DONE`, `compact0987.js:57`).
pub const TODO_STATUS_DONE: i64 = 1;
/// `TODO_TYPE.TODO` (`compact0293.js`: `{ ALL: 0, TODO: 1, FINISHED_TODO: 2 }`).
pub const TODO_TYPE_DEFAULT: i64 = 1;

/// One member blocked from rejoining a group (`/api/group/blockedmems/list`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockedGroupMember {
    /// Zalo user id.
    pub uid: String,
    /// Display name.
    pub name: String,
    /// Avatar URL (when provided).
    pub avatar: String,
}

/// Preview returned by `/api/group/link/ginfo` (`getGroupInfoByLink`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupLinkPreview {
    /// Bare group id (`groupId`).
    pub group_id: String,
    /// Group name (`name`).
    pub name: String,
    /// Description (`desc`).
    pub desc: String,
    /// Full avatar URL (`fullAvt` / `avt`).
    pub avatar: String,
    /// Total members (`totalMember`).
    pub total_member: u32,
    /// Sample member names (`currentMems[].dName`).
    pub sample_members: Vec<String>,
    /// Whether joining requires admin approval (`setting.joinAppr == 1`).
    pub join_approval: bool,
}

/// One server-synced pin entry from `/api/board/pin/list` or `/api/friendboard/list`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PinTopicEntry {
    /// Server topic / pin id.
    pub id: String,
    /// Underlying chat message `globalMsgId` (when pinned from a message).
    pub global_msg_id: Option<i64>,
    /// Preview title / message text.
    pub title: String,
    /// Raw topic JSON object (needed when reordering or multi-unpinning).
    pub raw: Value,
}

/// Input to create a Zalo To-Do task (`/api/board/personal/todo/create`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TodoCreateInput {
    /// Short task title (`content`).
    pub content: String,
    /// Optional detailed description (`description`).
    pub description: String,
    /// Assignee UIDs (`assignees`, JSON-encoded array of strings).
    pub assignees: Vec<String>,
    /// Optional due timestamp in ms (`dueDate`, `-1` if none).
    pub due_date: i64,
    /// Task status (`0` open, `2` done).
    pub status: i64,
    /// Optional extra JSON metadata (`extra`).
    pub extra: Value,
}

/// Parsed Zalo To-Do item from `/api/board/personal/list` or `/api/board/personal/get`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TodoItem {
    /// Server task id (`id`).
    pub id: String,
    /// Task title (`content`).
    pub content: String,
    /// Detailed description (`description`).
    pub description: String,
    /// Status (`0` open, `2` completed).
    pub status: i64,
    /// Due timestamp in ms (`0` or `-1` when unset).
    pub due_date: i64,
    /// Created / updated timestamp in ms.
    pub updated_at: i64,
    /// Creator uid (`creatorId`).
    pub creator_id: String,
    /// Assigned user ids (`assignees`).
    pub assignees: Vec<String>,
    /// Number of ding/reminders sent (`dingCount` / `reminderCount`).
    pub ding_count: i64,
}

// ── Pure payload builders & response parsers ──────────────────────────────

/// Payload for `/api/convlabel/manage/add` (`12163`, `compact0891.js:52` + `main-startup.js:K2`).
#[must_use]
pub fn label_manage_add_payload(
    text: &str,
    color: &str,
    emoji: &str,
    conversations: &[String],
) -> Value {
    let mut info = serde_json::Map::new();
    info.insert("text".to_owned(), json!(text));
    info.insert("color".to_owned(), json!(color));
    if !emoji.is_empty() {
        info.insert("emoji".to_owned(), json!(emoji));
    }
    json!({
        "labelInfo": Value::Object(info).to_string(),
        "add": conversations,
    })
}

/// Payload for `/api/convlabel/manage/update` (`12164`, `compact0891.js:63` + `main-startup.js:K2`).
#[must_use]
pub fn label_manage_update_payload(
    id: i64,
    text: &str,
    color: &str,
    offset: i64,
    add_convs: &[String],
    remove_convs: &[String],
) -> Value {
    let info = json!({
        "id": id,
        "text": text,
        "color": color,
        "offset": offset,
    });
    json!({
        "labelInfo": info.to_string(),
        "add": add_convs,
        "remove": remove_convs,
    })
}

/// Payload for `/api/convlabel/manage/delete` (`12165`).
#[must_use]
pub fn label_manage_delete_payload(label_ids: &[i64]) -> Value {
    json!({ "labelIds": label_ids })
}

/// Payload for `/api/convlabel/manage/reorder` (`12166`, `compact0891.js:81` + `main-startup.js:K2`).
#[must_use]
pub fn label_manage_reorder_payload(items: &[(i64, i64)]) -> Value {
    let list: Vec<Value> = items
        .iter()
        .map(|(id, offset)| json!({ "id": id, "offset": offset }))
        .collect();
    json!({ "lsLabelInfo": Value::Array(list).to_string() })
}

/// Payload for `/api/conv/blocksharing/updateConvers` (`12179`, `main-startup.js:z9`).
#[must_use]
pub fn block_sharing_update_payload(
    thread_id: &str,
    is_group: bool,
    enabled: bool,
    client_lang: &str,
) -> Value {
    json!({
        "threadId": strip_group(thread_id),
        "isGroup": if is_group { 1 } else { 0 },
        "isEnabled": enabled,
        "clientLang": client_lang,
    })
}

/// Normalizes a server block-sharing list (`/api/conv/blocksharing/getConvers`) into `uid` / `g_<id>` keys.
#[must_use]
pub fn block_sharing_ids(value: &Value) -> HashSet<String> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let mut out = HashSet::new();
    let Some(list) = root
        .get("convers")
        .or_else(|| root.get("conversations"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for item in list {
        let enabled = item
            .get("isEnabled")
            .and_then(|v| {
                v.as_bool()
                    .or_else(|| v.as_i64().map(|n| n != 0))
            })
            .unwrap_or(true);
        if !enabled {
            continue;
        }
        let raw_id = item
            .get("threadId")
            .or_else(|| item.get("convId"))
            .or_else(|| item.get("id"))
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            })
            .unwrap_or_default();
        if raw_id.is_empty() {
            continue;
        }
        let is_group = item
            .get("isGroup")
            .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)))
            .unwrap_or_else(|| raw_id.starts_with('g'));
        let bare = raw_id.trim_start_matches("g_").trim_start_matches('g');
        if is_group {
            out.insert(format!("g_{bare}"));
        } else {
            out.insert(bare.to_owned());
        }
    }
    out
}

/// Parses `/api/group/blockedmems/list` (`11811`) into [`BlockedGroupMember`] rows.
#[must_use]
pub fn parse_blocked_group_members(value: &Value) -> Vec<BlockedGroupMember> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let list = root
        .get("blocked_members")
        .or_else(|| root.get("blockedMembers"))
        .or_else(|| root.get("members"))
        .or_else(|| root.get("items"))
        .and_then(Value::as_array);
    let Some(list) = list else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        if let Some(uid_str) = item.as_str() {
            if !uid_str.is_empty() {
                out.push(BlockedGroupMember {
                    uid: uid_str.to_owned(),
                    name: uid_str.to_owned(),
                    avatar: String::new(),
                });
            }
            continue;
        }
        let uid = json_id_str(item, &["id", "uid", "userId"]);
        if uid.is_empty() {
            continue;
        }
        let name = json_text_any(item, &["dName", "displayName", "zaloName", "name"]);
        let avatar = json_text_any(item, &["avatar", "avt"]);
        out.push(BlockedGroupMember {
            name: if name.is_empty() { uid.clone() } else { name },
            uid,
            avatar,
        });
    }
    out
}

/// Parses `/api/group/link/ginfo` (`11824`) into a [`GroupLinkPreview`].
#[must_use]
pub fn parse_group_link_preview(value: &Value) -> Option<GroupLinkPreview> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let name = json_text_any(root, &["name", "groupName"]);
    let group_id = json_id_str(root, &["groupId", "grid", "id"]);
    if name.is_empty() && group_id.is_empty() {
        return None;
    }
    let desc = json_text_any(root, &["desc", "description"]);
    let avatar = json_text_any(root, &["fullAvt", "avt", "avatar"]);
    let total_member = root
        .get("totalMember")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0) as u32;
    let join_approval = root
        .pointer("/setting/joinAppr")
        .and_then(|v| v.as_i64().or_else(|| v.as_bool().map(i64::from)))
        .is_some_and(|n| n != 0);
    let sample_members = root
        .get("currentMems")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let n = json_text_any(m, &["dName", "zaloName", "displayName"]);
                    (!n.is_empty()).then_some(n)
                })
                .take(6)
                .collect()
        })
        .unwrap_or_default();
    Some(GroupLinkPreview {
        group_id,
        name: if name.is_empty() {
            "Nhóm Zalo".to_owned()
        } else {
            name
        },
        desc,
        avatar,
        total_member,
        sample_members,
        join_approval,
    })
}

/// Parses `/api/board/pin/list` (`12001`) or `/api/friendboard/list` (`12065`) into [`PinTopicEntry`]s.
#[must_use]
pub fn parse_pin_topics(value: &Value) -> (Vec<PinTopicEntry>, i64) {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let version = root
        .get("boardVersion")
        .or_else(|| root.get("version"))
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0);
    let list = root
        .get("topics")
        .or_else(|| root.get("items"))
        .or_else(|| root.get("data"))
        .and_then(Value::as_array);
    let Some(list) = list else {
        return (Vec::new(), version);
    };
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let id = json_id_str(item, &["id", "topicId", "pinId"]);
        let params_obj = match item.get("params") {
            Some(Value::String(raw)) => serde_json::from_str::<Value>(raw).unwrap_or(Value::Null),
            Some(other) => other.clone(),
            None => Value::Null,
        };
        let gmi = params_obj
            .get("global_msg_id")
            .or_else(|| params_obj.get("globalMsgId"))
            .or_else(|| item.get("globalMsgId"))
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .filter(|&n| n > 0);
        let title = {
            let t = json_text_any(&params_obj, &["title", "content", "msg"]);
            if !t.is_empty() {
                t
            } else {
                json_text_any(item, &["title", "content", "msg"])
            }
        };
        out.push(PinTopicEntry {
            id,
            global_msg_id: gmi,
            title,
            raw: item.clone(),
        });
    }
    (out, version)
}

/// Payload for `/api/board/personal/todo/create` (`12413`, `compact3178.js:5080`).
#[must_use]
pub fn todo_create_payload(input: &TodoCreateInput, src: i64, imei: &str) -> Value {
    let extra = if input.extra.is_object() {
        input.extra.to_string()
    } else {
        "{}".to_owned()
    };
    json!({
        "assignees": json!(input.assignees).to_string(),
        "dueDate": input.due_date,
        "content": input.content,
        "description": input.description,
        "extra": extra,
        "dateDefaultType": 0,
        "status": input.status,
        "watchers": [],
        "src": src,
        "imei": imei,
    })
}

/// Parses `/api/board/personal/todo/verify`, `/api/board/personal/list`, or
/// `/api/board/personal/get` into [`TodoItem`]s.
#[must_use]
pub fn parse_todo_list(value: &Value) -> Vec<TodoItem> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let items: Vec<&Value> = if let Some(arr) = root
        .get("updated")
        .or_else(|| root.get("tasks"))
        .or_else(|| root.get("items"))
        .or_else(|| root.get("todos"))
        .or_else(|| root.get("data"))
        .and_then(Value::as_array)
    {
        arr.iter().collect()
    } else if root.get("id").is_some() && root.get("content").is_some() {
        vec![root]
    } else if let Some(arr) = value.as_array() {
        arr.iter().collect()
    } else {
        Vec::new()
    };
    items.into_iter().filter_map(parse_todo_one).collect()
}

fn parse_todo_one(v: &Value) -> Option<TodoItem> {
    let id = json_id_str(v, &["id", "taskId", "todoId"]);
    let content = json_text_any(v, &["content", "title"]);
    if id.is_empty() && content.is_empty() {
        return None;
    }
    let description = json_text_any(v, &["description", "desc"]);
    let status = v
        .get("status")
        .and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(TODO_STATUS_OPEN);
    let due_date = v
        .get("dueDate")
        .or_else(|| v.get("due_date"))
        .and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0);
    let updated_at = v
        .get("updatedTime")
        .or_else(|| v.get("updateTime"))
        .or_else(|| v.get("createdTime"))
        .or_else(|| v.get("createTime"))
        .and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0);
    let creator_id = json_id_str(v, &["creatorId", "creator"]);
    let ding_count = v
        .get("dingCount")
        .or_else(|| v.get("reminderCount"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let assignees_val = match v.get("assignees") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    };
    let assignees = assignees_val
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    a.as_str()
                        .map(str::to_owned)
                        .or_else(|| a.as_i64().map(|n| n.to_string()))
                        .or_else(|| {
                            let id = json_id_str(a, &["id", "uid", "userId"]);
                            (!id.is_empty()).then_some(id)
                        })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(TodoItem {
        id,
        content,
        description,
        status,
        due_date,
        updated_at,
        creator_id,
        assignees,
        ding_count,
    })
}

fn json_id_str(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| {
            v.get(*k).and_then(|x| {
                x.as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .or_else(|| x.as_i64().map(|n| n.to_string()))
            })
        })
        .unwrap_or_default()
}

fn json_text_any(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_owned()
}

// ── ZaloApi methods ───────────────────────────────────────────────────────

impl ZaloApi {
    fn label_manage_base(&self) -> String {
        self.service_base("label", "https://tt-label-wpa.chat.zalo.me")
            .to_owned()
    }

    fn friend_board_base(&self) -> String {
        self.service_base("friend_board", "https://tt-friendboard-wpa.chat.zalo.me")
            .to_owned()
    }

    fn personal_board_base(&self) -> String {
        self.service_base("boards", "https://tt-board-wpa.chat.zalo.me")
            .to_owned()
    }

    fn group_board_host(&self) -> String {
        self.service_base("group_board", "https://tt-groupboard-wpa.chat.zalo.me")
            .to_owned()
    }

    fn group_poll_host(&self) -> String {
        self.service_base("group_poll", "https://tt-group-wpa.chat.zalo.me")
            .to_owned()
    }

    // ── 1. Labels Management 26.11.12 ───────────────────────────────────

    /// Creates a new conversation label (`/api/convlabel/manage/add`, cmd `12163`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn create_label_v2(
        &self,
        text: &str,
        color: &str,
        emoji: &str,
        conversations: &[String],
    ) -> Result<Value, ClientError> {
        let payload = label_manage_add_payload(text, color, emoji, conversations);
        self.post_encrypted(&self.label_manage_base(), "/api/convlabel/manage/add", &payload)
            .await
    }

    /// Updates a conversation label's text/color/assignments (`/api/convlabel/manage/update`, cmd `12164`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_label_v2(
        &self,
        id: i64,
        text: &str,
        color: &str,
        offset: i64,
        add_convs: &[String],
        remove_convs: &[String],
    ) -> Result<Value, ClientError> {
        let payload = label_manage_update_payload(id, text, color, offset, add_convs, remove_convs);
        self.post_encrypted(
            &self.label_manage_base(),
            "/api/convlabel/manage/update",
            &payload,
        )
        .await
    }

    /// Deletes one or more conversation labels (`/api/convlabel/manage/delete`, cmd `12165`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn delete_labels_v2(&self, label_ids: &[i64]) -> Result<Value, ClientError> {
        let payload = label_manage_delete_payload(label_ids);
        self.post_encrypted(
            &self.label_manage_base(),
            "/api/convlabel/manage/delete",
            &payload,
        )
        .await
    }

    /// Reorders conversation labels on the server (`/api/convlabel/manage/reorder`, cmd `12166`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn reorder_labels_v2(&self, items: &[(i64, i64)]) -> Result<Value, ClientError> {
        let payload = label_manage_reorder_payload(items);
        self.post_encrypted(
            &self.label_manage_base(),
            "/api/convlabel/manage/reorder",
            &payload,
        )
        .await
    }

    // ── 2. Block Sharing 26.11.12 ─────────────────────────────────────────

    /// Fetches conversations with Block Sharing enabled (`/api/conv/blocksharing/getConvers`, cmd `12178`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_block_sharing_convers(&self) -> Result<Value, ClientError> {
        let host = self.conversation_base().to_owned();
        self.get_encrypted(&host, "/api/conv/blocksharing/getConvers", &json!({}))
            .await
    }

    /// Toggles Block Sharing on a 1-on-1 or group conversation
    /// (`/api/conv/blocksharing/updateConvers`, cmd `12179`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_block_sharing_conver(
        &self,
        thread_id: &str,
        is_group: bool,
        enabled: bool,
        client_lang: &str,
    ) -> Result<Value, ClientError> {
        let host = self.conversation_base().to_owned();
        let payload = block_sharing_update_payload(thread_id, is_group, enabled, client_lang);
        self.post_encrypted(&host, "/api/conv/blocksharing/updateConvers", &payload)
            .await
    }

    // ── 3. Advanced Group Management ──────────────────────────────────────

    /// Lists members blocked from rejoining a group (`/api/group/blockedmems/list`, cmd `11811`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_blocked_members(
        &self,
        grid: &str,
        page: u32,
        count: u32,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "page": page,
            "count": count,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_base(), "/api/group/blockedmems/list", &payload)
            .await
    }

    /// Blocks one or more users from rejoining a group (`/api/group/blockedmems/add`, cmd `11812`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn block_group_members(
        &self,
        grid: &str,
        members: &[String],
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "members": members,
        });
        self.get_encrypted(&self.group_base(), "/api/group/blockedmems/add", &payload)
            .await
    }

    /// Unblocks one or more users from a group (`/api/group/blockedmems/remove`, cmd `11813`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn unblock_group_members(
        &self,
        grid: &str,
        members: &[String],
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "members": members,
        });
        self.get_encrypted(
            &self.group_base(),
            "/api/group/blockedmems/remove",
            &payload,
        )
        .await
    }

    /// Fetches group preview metadata for an invite link `https://zalo.me/g/...`
    /// (`/api/group/link/ginfo`, cmd `11824`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_info_by_link(
        &self,
        link: &str,
        mpage: u32,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "link": link,
            "avatar_size": AVATAR_SIZE,
            "member_avatar_size": AVATAR_SIZE,
            "mpage": mpage,
        });
        self.get_encrypted(&self.group_base(), "/api/group/link/ginfo", &payload)
            .await
    }

    /// Joins a group using its invite link (`/api/group/link/join`, cmd `11825`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn join_group_by_link(
        &self,
        link: &str,
        client_lang: &str,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "link": link,
            "clientLang": client_lang,
        });
        self.get_encrypted(&self.group_base(), "/api/group/link/join", &payload)
            .await
    }

    /// Invites one member into multiple groups at once (`/api/group/invite/multi`, cmd `12605`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn invite_member_multi(
        &self,
        grids: &[String],
        member: &str,
        member_type: i64,
        src_interaction: i64,
        client_lang: &str,
    ) -> Result<Value, ClientError> {
        let bare: Vec<&str> = grids.iter().map(|g| strip_group(g)).collect();
        let payload = json!({
            "grids": bare,
            "member": member,
            "memberType": member_type,
            "srcInteraction": src_interaction,
            "clientLang": client_lang,
        });
        self.get_encrypted(&self.group_base(), "/api/group/invite/multi", &payload)
            .await
    }

    /// Kicks one member out of multiple groups at once (`/api/group/kickout/multi`, cmd `12606`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn kickout_member_multi(
        &self,
        grids: &[String],
        member: &str,
        src_interaction: i64,
    ) -> Result<Value, ClientError> {
        let bare: Vec<&str> = grids.iter().map(|g| strip_group(g)).collect();
        let payload = json!({
            "grids": bare,
            "member": member,
            "srcInteraction": src_interaction,
        });
        self.get_encrypted(&self.group_base(), "/api/group/kickout/multi", &payload)
            .await
    }

    /// Fetches detailed info for a pending group invitation (`/api/group/inv-box/inv-info`, cmd `12173`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_invitation_info(
        &self,
        grid: &str,
        mcount: u32,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grId": strip_group(grid),
            "mcount": mcount,
            "mpage": 1,
        });
        self.get_encrypted(&self.group_base(), "/api/group/inv-box/inv-info", &payload)
            .await
    }

    /// Fetches group activity/member analytics (`/api/group/analyse`, cmd `12407`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_analyse(&self, grid: &str) -> Result<Value, ClientError> {
        let payload = json!({ "grid": strip_group(grid) });
        self.get_encrypted(&self.group_base(), "/api/group/analyse", &payload)
            .await
    }

    /// Removes the legacy group topic (`/api/group/topic/remove`, cmd `11829`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn remove_group_topic_legacy(&self, grid: &str) -> Result<Value, ClientError> {
        let payload = json!({ "grid": strip_group(grid) });
        self.post_encrypted(&self.group_base(), "/api/group/topic/remove", &payload)
            .await
    }

    /// Checks community member quota for adding a user (`/api/group/quota/members-comm`, cmd `12182`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn check_community_member_quota(
        &self,
        grid: &str,
        uid: &str,
        language: &str,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "language": language,
            "grId": strip_group(grid),
            "uid": uid,
        });
        self.get_encrypted(
            &self.group_base(),
            "/api/group/quota/members-comm",
            &payload,
        )
        .await
    }

    // ── 4. Group Multi-Pin, Reminders, 1-1 FriendBoard & Poll Share ──────

    /// Lists pinned topics of a group (`/api/board/pin/list`, cmd `12001`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn list_pin_topics(
        &self,
        grid: &str,
        board_version: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "groupId": strip_group(grid),
            "boardVersion": board_version,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_board_host(), "/api/board/pin/list", &payload)
            .await
    }

    /// Reorders pinned topics in a group (`/api/board/pin/update`, cmd `12002`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn reorder_pin_topics(
        &self,
        grid: &str,
        topics: &[Value],
        board_version: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "groupId": strip_group(grid),
            "topics": topics,
            "boardVersion": board_version,
        });
        self.post_encrypted(&self.group_board_host(), "/api/board/pin/update", &payload)
            .await
    }

    /// Unpins multiple topics in a group at once (`/api/board/multi_unpin`, cmd `12010`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn multi_unpin_board_topics(
        &self,
        grid: &str,
        topics: &[Value],
        board_version: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "topics": topics,
            "boardVersion": board_version,
        });
        self.get_encrypted(&self.group_board_host(), "/api/board/multi_unpin", &payload)
            .await
    }

    /// Lists group reminders (`/api/board/listReminder`, cmd `12425`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn list_group_reminders(
        &self,
        grid: &str,
        page: u32,
        count: u32,
        last_id: i64,
        last_type: i64,
    ) -> Result<Value, ClientError> {
        let object_data = json!({
            "group_id": strip_group(grid),
            "board_type": 0,
            "page": page,
            "count": count,
            "last_id": last_id,
            "last_type": last_type,
        });
        let payload = json!({
            "objectData": object_data.to_string(),
            "imei": self.device.imei,
        });
        self.get_encrypted(
            &self.group_board_host(),
            "/api/board/listReminder",
            &payload,
        )
        .await
    }

    /// Updates an existing 1-on-1 reminder (`/api/board/oneone/update`, cmd `12430`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_oneone_reminder(&self, object_data: &Value) -> Result<Value, ClientError> {
        let payload = json!({ "objectData": object_data.to_string() });
        self.post_encrypted(
            &self.group_board_host(),
            "/api/board/oneone/update",
            &payload,
        )
        .await
    }

    /// Lists server-synced pinned messages in a 1-on-1 chat (`/api/friendboard/list`, cmd `12065`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn list_friend_board_pins(
        &self,
        conversation_id: &str,
        version: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "conversationId": conversation_id,
            "version": version,
        });
        self.get_encrypted(&self.friend_board_base(), "/api/friendboard/list", &payload)
            .await
    }

    /// Creates a server-synced pinned message in a 1-on-1 chat (`/api/friendboard/create`, cmd `12067`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn create_friend_board_pin(
        &self,
        conversation_id: &str,
        topic: &Value,
        version: i64,
        lang: &str,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "conversationId": conversation_id,
            "topic": topic,
            "version": version,
            "lang": lang,
        });
        self.get_encrypted(
            &self.friend_board_base(),
            "/api/friendboard/create",
            &payload,
        )
        .await
    }

    /// Reorders pinned messages in a 1-on-1 chat (`/api/friendboard/reorder`, cmd `12068`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn reorder_friend_board_pins(
        &self,
        conversation_id: &str,
        topics: &[Value],
        version: i64,
        lang: &str,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "conversationId": conversation_id,
            "topics": topics,
            "version": version,
            "lang": lang,
        });
        self.get_encrypted(
            &self.friend_board_base(),
            "/api/friendboard/reorder",
            &payload,
        )
        .await
    }

    /// Unpins one or more messages in a 1-on-1 chat (`/api/friendboard/multi_unpin`, cmd `12069`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn multi_unpin_friend_board(
        &self,
        conversation_id: &str,
        topics: &[Value],
        version: i64,
        lang: &str,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "conversationId": conversation_id,
            "topics": topics,
            "version": version,
            "lang": lang,
        });
        self.get_encrypted(
            &self.friend_board_base(),
            "/api/friendboard/multi_unpin",
            &payload,
        )
        .await
    }

    /// Shares an existing group poll back into the conversation (`/api/poll/share`, cmd `12085`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn share_poll(&self, poll_id: i64) -> Result<Value, ClientError> {
        let payload = json!({
            "poll_id": poll_id,
            "imei": self.device.imei,
        });
        self.post_encrypted(&self.group_poll_host(), "/api/poll/share", &payload)
            .await
    }

    // ── 5. Zalo To-Do (Personal Board — 9 APIs) ───────────────────────────

    /// Creates a personal To-Do task (`/api/board/personal/todo/create`, cmd `12413`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn todo_create(
        &self,
        input: &TodoCreateInput,
        src: i64,
    ) -> Result<Value, ClientError> {
        let payload = todo_create_payload(input, src, &self.device.imei);
        self.post_encrypted(
            &self.personal_board_base(),
            "/api/board/personal/todo/create",
            &payload,
        )
        .await
    }

    /// Updates an existing personal To-Do task (`/api/board/personal/todo/update`, cmd `12414`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn todo_update(&self, task_payload: &Value) -> Result<Value, ClientError> {
        self.post_encrypted(
            &self.personal_board_base(),
            "/api/board/personal/todo/update",
            task_payload,
        )
        .await
    }

    /// Deletes a personal To-Do task (`/api/board/personal/delete`, cmd `12415`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn todo_delete(&self, id: &str) -> Result<Value, ClientError> {
        let payload = json!({ "id": id });
        self.get_encrypted(
            &self.personal_board_base(),
            "/api/board/personal/delete",
            &payload,
        )
        .await
    }

    /// Lists personal To-Do tasks (`/api/board/personal/list`, cmd `12416`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn todo_list(
        &self,
        scroll_id: &str,
        queries: &Value,
        offset: u32,
        limit: u32,
        todo_type: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "scrollId": scroll_id,
            "offset": offset,
            "limit": limit,
            "queries": queries.to_string(),
            "type": todo_type,
        });
        self.get_encrypted(
            &self.personal_board_base(),
            "/api/board/personal/list",
            &payload,
        )
        .await
    }

    /// Fetches one To-Do task's detail (`/api/board/personal/get`, cmd `12417`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn todo_get(&self, id: &str) -> Result<Value, ClientError> {
        let payload = json!({
            "id": id,
            "imei": self.device.imei,
        });
        self.get_encrypted(
            &self.personal_board_base(),
            "/api/board/personal/get",
            &payload,
        )
        .await
    }

    /// Updates a To-Do task's completion status (`/api/board/personal/todo/status`, cmd `12418`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn todo_update_status(&self, id: &str, status: i64) -> Result<Value, ClientError> {
        let payload = json!({
            "id": id,
            "status": status,
            "imei": self.device.imei,
        });
        self.get_encrypted(
            &self.personal_board_base(),
            "/api/board/personal/todo/status",
            &payload,
        )
        .await
    }

    /// Sends a "Ding" reminder nudge for one or more To-Do tasks (`/api/board/personal/todo/ding`, cmd `12426`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn todo_ding(&self, task_ids: &[String]) -> Result<Value, ClientError> {
        let payload = json!({ "taskIds": task_ids });
        self.get_encrypted(
            &self.personal_board_base(),
            "/api/board/personal/todo/ding",
            &payload,
        )
        .await
    }

    /// Acknowledges received "Ding" reminders (`/api/board/personal/todo/dingack`, cmd `12427`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn todo_ding_ack(&self, task_ids: &[String]) -> Result<Value, ClientError> {
        let payload = json!({ "taskIds": task_ids });
        self.get_encrypted(
            &self.personal_board_base(),
            "/api/board/personal/todo/dingack",
            &payload,
        )
        .await
    }

    /// Verifies local To-Do sync state against the server (`/api/board/personal/todo/verify`, cmd `12603`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn todo_verify(
        &self,
        scroll_id: &str,
        last_updated_time: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "scrollId": scroll_id,
            "lastUpdatedTime": last_updated_time,
            "imei": self.device.imei,
        });
        self.get_encrypted(
            &self.personal_board_base(),
            "/api/board/personal/todo/verify",
            &payload,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_manage_26_11_12_payloads_match_zalo_pc() {
        let add = label_manage_add_payload("VIP", "#d91b1b", "", &["g10".into()]);
        let add_info: Value = serde_json::from_str(add["labelInfo"].as_str().unwrap()).unwrap();
        assert_eq!(add_info["text"], "VIP");
        assert_eq!(add_info["color"], "#d91b1b");
        assert_eq!(add["add"][0], "g10");

        let upd = label_manage_update_payload(
            2,
            "Khách cũ",
            "#4bc377",
            1,
            &["u1".into()],
            &["u2".into()],
        );
        let upd_info: Value = serde_json::from_str(upd["labelInfo"].as_str().unwrap()).unwrap();
        assert_eq!(upd_info["id"], 2);
        assert_eq!(upd_info["offset"], 1);
        assert_eq!(upd["add"][0], "u1");
        assert_eq!(upd["remove"][0], "u2");

        let del = label_manage_delete_payload(&[2, 5]);
        assert_eq!(del["labelIds"], json!([2, 5]));

        let re = label_manage_reorder_payload(&[(2, 0), (1, 1)]);
        let ls: Value = serde_json::from_str(re["lsLabelInfo"].as_str().unwrap()).unwrap();
        assert_eq!(ls[0]["id"], 2);
        assert_eq!(ls[1]["offset"], 1);
    }

    #[test]
    fn block_sharing_payload_and_parser_match_zalo_pc() {
        let p = block_sharing_update_payload("g_55", true, true, "vi");
        assert_eq!(p["threadId"], "55");
        assert_eq!(p["isGroup"], 1);
        assert_eq!(p["isEnabled"], true);

        let resp = json!({
            "data": {
                "convers": [
                    { "threadId": "55", "isGroup": 1, "isEnabled": true },
                    { "threadId": "1001", "isGroup": 0, "isEnabled": 1 },
                    { "threadId": "2002", "isGroup": 0, "isEnabled": false }
                ]
            }
        });
        let ids = block_sharing_ids(&resp);
        assert!(ids.contains("g_55"));
        assert!(ids.contains("1001"));
        assert!(!ids.contains("2002"));
    }

    #[test]
    fn group_blocked_link_preview_pin_and_todo_parsers_work() {
        let blocked = parse_blocked_group_members(&json!({
            "data": {
                "blocked_members": [
                    { "id": "33", "dName": "Người bị chặn", "avatar": "https://a.zdn.vn/1.jpg" }
                ]
            }
        }));
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].uid, "33");
        assert_eq!(blocked[0].name, "Người bị chặn");

        let preview = parse_group_link_preview(&json!({
            "data": {
                "groupId": "789",
                "name": "Nhóm Dev",
                "desc": "Trao đổi Rust",
                "fullAvt": "https://a.zdn.vn/g.jpg",
                "totalMember": 12,
                "setting": { "joinAppr": 1 },
                "currentMems": [{ "dName": "An" }, { "dName": "Bình" }]
            }
        }))
        .unwrap();
        assert_eq!(preview.group_id, "789");
        assert_eq!(preview.total_member, 12);
        assert!(preview.join_approval);
        assert_eq!(preview.sample_members, vec!["An", "Bình"]);

        let (pins, ver) = parse_pin_topics(&json!({
            "data": {
                "boardVersion": 4,
                "topics": [{
                    "id": "p1",
                    "params": "{\"global_msg_id\":\"9988\",\"title\":\"Thông báo\"}"
                }]
            }
        }));
        assert_eq!(ver, 4);
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].global_msg_id, Some(9988));
        assert_eq!(pins[0].title, "Thông báo");

        let tc = todo_create_payload(
            &TodoCreateInput {
                content: "Kiểm tra bản build".into(),
                description: "Nhóm 3".into(),
                assignees: vec!["101".into()],
                due_date: 1_750_000_000_000,
                status: TODO_STATUS_OPEN,
                extra: Value::Null,
            },
            -1,
            "imei-1",
        );
        assert_eq!(tc["content"], "Kiểm tra bản build");
        assert_eq!(tc["assignees"], "[\"101\"]");

        let todos = parse_todo_list(&json!({
            "data": {
                "tasks": [{
                    "id": "t1",
                    "content": "Giao hàng",
                    "description": "Gấp",
                    "status": 0,
                    "dueDate": 1750000000000_i64,
                    "creatorId": "50",
                    "assignees": "[\"50\",\"51\"]",
                    "dingCount": 2
                }]
            }
        }));
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].id, "t1");
        assert_eq!(todos[0].assignees, vec!["50", "51"]);
        assert_eq!(todos[0].ding_count, 2);

        let verified = parse_todo_list(&json!({
            "data": {
                "scrollId": "s2",
                "updated": [{ "id": "t2", "content": "Review PR", "status": 1 }]
            }
        }));
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].id, "t2");
        assert_eq!(verified[0].status, TODO_STATUS_DONE);
    }
}
