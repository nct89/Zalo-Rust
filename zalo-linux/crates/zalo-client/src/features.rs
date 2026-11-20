//! Extended Zalo PC APIs: aliases, friend/group management, invites,
//! unread marks, delete conversation, abuse report, board/reminders/polls,
//! personalized sticker store, and 2FA settings.

use serde_json::{json, Value};

use super::api::{ClientError, ZaloApi};
use super::ops::strip_group;

/// Board / topic category constants from Zalo PC module `1pet`.
pub const TOPIC_TEXT: i64 = 0;
/// Reminder topic (`REMINDER_TOPIC` in `1pet`).
pub const TOPIC_REMINDER: i64 = 4;
/// `EVENT_STATUS.NONE` — the member has not answered a group reminder.
pub const EVENT_NONE: i64 = 0;
/// `EVENT_STATUS.ACCEPT` — accept / attend.
pub const EVENT_ACCEPT: i64 = 1;
/// `EVENT_STATUS.REJECT` — decline.
pub const EVENT_REJECT: i64 = 2;
/// Board list filter `ALL` (`GROUP_BOARD_ITEM_TYPE.ALL`).
pub const BOARD_ALL: i64 = 0;
/// Abuse report target type: 1-on-1 user.
pub const REPORT_TYPE_USER: i64 = 2;
/// Abuse report target type: group.
pub const REPORT_TYPE_GROUP: i64 = 14;

/// Metadata of the last message in a thread for `addUnreadMark` / `deleteconver`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadMsgRef {
    /// Conversation id (`thread_id`, e.g. `"g_123"` or `"456"`).
    pub thread_id: String,
    /// True for group threads.
    pub is_group: bool,
    /// Last message's `cliMsgId` (string on wire).
    pub cli_msg_id: String,
    /// Last message's `globalMsgId` (needed for `deleteconver`).
    pub global_msg_id: String,
    /// Sender uid of the last message.
    pub from_uid: String,
    /// Timestamp (ms) of the last message (or current time).
    pub ts: i64,
}

/// Group permission toggles accepted by `/api/group/setting/update`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupSettingsPatch {
    /// Lock sending messages to admins/creator (`0`/`1`).
    pub lock_send_msg: Option<i64>,
    /// Require admin approval for new joiners (`0`/`1`).
    pub join_appr: Option<i64>,
    /// Block members from changing group name/avatar (`0`/`1`).
    pub block_name: Option<i64>,
    /// Highlight admin/creator messages (`0`/`1`).
    pub sign_admin_msg: Option<i64>,
    /// Restrict member list visibility (`0`/`1`).
    pub lock_view_member: Option<i64>,
    /// Restrict creating polls (`0`/`1`).
    pub lock_create_poll: Option<i64>,
    /// Restrict creating notes/reminders (`0`/`1`).
    pub lock_create_post: Option<i64>,
}

/// Builds encrypted payload JSON for `/api/conv/addUnreadMark`.
#[must_use]
pub fn add_unread_payload(item: &ThreadMsgRef, imei: &str) -> Value {
    let entry = json!({
        "id": strip_group(&item.thread_id),
        "cliMsgId": item.cli_msg_id,
        "fromUid": item.from_uid,
        "ts": item.ts,
    });
    let mut inner = json!({
        "convsGroup": [],
        "convsUser": [],
        "imei": imei,
    });
    if item.is_group {
        inner["convsGroup"] = json!([entry]);
    } else {
        inner["convsUser"] = json!([entry]);
    }
    json!({ "param": inner.to_string() })
}

/// Builds encrypted payload JSON for `/api/conv/removeUnreadMark`.
#[must_use]
pub fn remove_unread_payload(thread_id: &str, is_group: bool, ts: i64) -> Value {
    let raw_id = strip_group(thread_id);
    let data = json!([{ "id": raw_id, "ts": ts }]);
    let inner = if is_group {
        json!({
            "convsGroup": [raw_id],
            "convsUser": [],
            "convsGroupData": data,
            "convsUserData": [],
        })
    } else {
        json!({
            "convsGroup": [],
            "convsUser": [raw_id],
            "convsGroupData": [],
            "convsUserData": data,
        })
    };
    json!({ "param": inner.to_string() })
}

/// Builds payload for `/api/message/deleteconver` or `/api/group/deleteconver`.
#[must_use]
pub fn delete_conver_payload(item: &ThreadMsgRef, client_id: u64, imei: &str) -> Value {
    let conver = json!({
        "ownerId": item.from_uid,
        "cliMsgId": item.cli_msg_id,
        "globalMsgId": item.global_msg_id,
    });
    if item.is_group {
        json!({
            "grid": strip_group(&item.thread_id),
            "cliMsgId": client_id.to_string(),
            "conver": conver,
            "onlyMe": 1,
            "imei": imei,
        })
    } else {
        json!({
            "toid": item.thread_id,
            "cliMsgId": client_id.to_string(),
            "conver": conver,
            "onlyMe": 1,
            "imei": imei,
        })
    }
}

/// Builds payload for `/api/group/setting/update`.
#[must_use]
pub fn group_settings_payload(grid: &str, patch: &GroupSettingsPatch, imei: &str) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("grid".to_owned(), Value::from(strip_group(grid)));
    map.insert("imei".to_owned(), Value::from(imei));
    for (key, val) in [
        ("lockSendMsg", patch.lock_send_msg),
        ("joinAppr", patch.join_appr),
        ("blockName", patch.block_name),
        ("signAdminMsg", patch.sign_admin_msg),
        ("lockViewMember", patch.lock_view_member),
        ("lockCreatePoll", patch.lock_create_poll),
        ("lockCreatePost", patch.lock_create_post),
    ] {
        if let Some(v) = val {
            map.insert(key.to_owned(), Value::from(v));
        }
    }
    Value::Object(map)
}

/// Builds payload for `/api/board/topic/createv2`.
#[must_use]
pub fn group_topic_payload(
    grid: &str,
    topic_type: i64,
    title: &str,
    start_time: i64,
    repeat: i64,
    pin_act: i64,
    imei: &str,
) -> Value {
    json!({
        "grid": strip_group(grid),
        "type": topic_type,
        "color": -16_777_216_i64,
        "emoji": if topic_type == TOPIC_REMINDER { "⏰" } else { "" },
        "startTime": start_time,
        "duration": -1,
        "params": json!({ "title": title }).to_string(),
        "pinAct": pin_act,
        "repeat": repeat,
        "src": 1,
        "imei": imei,
    })
}

/// Builds payload for `/api/board/topic/updatev2`.
///
/// Electron `updateGroupTopicv2` drops `needPin` on this path and sends
/// `pinAct` (`1` pin, `2` leave unpinned) plus `topicId`.
#[must_use]
pub fn group_topic_update_payload(
    grid: &str,
    topic_id: &str,
    topic_type: i64,
    title: &str,
    start_time: i64,
    repeat: i64,
    pin_act: i64,
    imei: &str,
) -> Value {
    json!({
        "grid": strip_group(grid),
        "type": topic_type,
        "color": -16_777_216_i64,
        "emoji": if topic_type == TOPIC_REMINDER { "⏰" } else { "" },
        "startTime": if start_time == 0 { -1 } else { start_time },
        "duration": -1,
        "params": json!({ "title": title }).to_string(),
        "topicId": topic_id,
        "repeat": repeat,
        "pinAct": pin_act,
        "imei": imei,
    })
}

/// Builds payload for `/api/board/topic/updateEventResponse`.
///
/// `status` is [`EVENT_ACCEPT`] or [`EVENT_REJECT`]. A numeric event id is
/// sent as a JSON number, matching `JSON.stringify` of Electron's `content.id`.
#[must_use]
pub fn event_response_payload(grid: &str, status: i64, event_id: &str, imei: &str) -> Value {
    json!({
        "grid": strip_group(grid),
        "type": status,
        "eventId": event_id_value(event_id),
        "imei": imei,
    })
}

/// Builds payload for `/api/board/topic/listResponseEvent` (cmd 12086).
///
/// Electron `getListResponseEvent` sends only `{eventId}`.
#[must_use]
pub fn list_response_event_payload(event_id: &str) -> Value {
    json!({ "eventId": event_id_value(event_id) })
}

/// Builds payload for `/api/board/topic/getReminder` (cmd 12088).
///
/// Electron `getDetailReminder` sends `{eventId, imei}`.
#[must_use]
pub fn reminder_detail_payload(event_id: &str, imei: &str) -> Value {
    json!({
        "eventId": event_id_value(event_id),
        "imei": imei,
    })
}

/// A numeric event id is a JSON number, matching `JSON.stringify` of Electron's id.
fn event_id_value(event_id: &str) -> Value {
    event_id
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::String(event_id.to_owned()))
}

/// Builds payload for `/api/board/oneone/create`.
#[must_use]
pub fn oneone_reminder_payload(
    to_uid: &str,
    title: &str,
    start_time: i64,
    repeat: i64,
    creator_uid: &str,
    imei: &str,
) -> Value {
    let obj = json!({
        "toUid": to_uid,
        "type": TOPIC_REMINDER,
        "color": -16_777_216_i64,
        "emoji": "⏰",
        "startTime": start_time,
        "duration": -1,
        "params": json!({ "title": title }),
        "needPin": false,
        "repeat": repeat,
        "creatorUid": creator_uid,
        "src": 1,
    });
    json!({
        "objectData": obj.to_string(),
        "imei": imei,
    })
}

/// Builds payload for `/api/poll/create`.
#[must_use]
pub fn create_poll_payload(
    grid: &str,
    question: &str,
    options: &[String],
    allow_multi: bool,
    allow_add: bool,
    is_anonymous: bool,
    imei: &str,
) -> Value {
    json!({
        "group_id": strip_group(grid),
        "question": question,
        "options": options,
        "allow_multi_choices": allow_multi,
        "allow_add_new_option": allow_add,
        "is_anonymous": is_anonymous,
        "poll_type": 0,
        "expired_time": 0,
        "pinAct": 1,
        "src": 1,
        "imei": imei,
    })
}

impl ZaloApi {
    fn alias_base(&self) -> String {
        self.service_base("alias", "https://tt-alias-wpa.chat.zalo.me")
            .to_owned()
    }

    fn group_board_base(&self) -> String {
        self.service_base("group_board", "https://tt-groupboard-wpa.chat.zalo.me")
            .to_owned()
    }

    fn group_poll_base(&self) -> String {
        self.service_base("group_poll", "https://tt-group-wpa.chat.zalo.me")
            .to_owned()
    }

    // ── Friend Alias & Removal ────────────────────────────────────────────

    /// Lists friend aliases (`/api/alias/list`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_aliases(&self, page: u32, count: u32) -> Result<Value, ClientError> {
        let payload = json!({
            "page": page,
            "count": count,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.alias_base(), "/api/alias/list", &payload)
            .await
    }

    /// Sets or updates a friend's alias (`/api/alias/update`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_alias(&self, friend_id: &str, alias: &str) -> Result<Value, ClientError> {
        let payload = json!({
            "friendId": friend_id,
            "alias": alias,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.alias_base(), "/api/alias/update", &payload)
            .await
    }

    /// Removes a friend's alias (`/api/alias/remove`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn remove_alias(&self, friend_id: &str) -> Result<Value, ClientError> {
        let payload = json!({ "friendId": friend_id });
        self.get_encrypted(&self.alias_base(), "/api/alias/remove", &payload)
            .await
    }

    /// Unfriends a contact (`/api/friend/remove`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn remove_friend(&self, fid: &str) -> Result<Value, ClientError> {
        let payload = json!({
            "fid": fid,
            "imei": self.device.imei,
        });
        let base = self.friend_base().to_owned();
        self.post_encrypted(&base, "/api/friend/remove", &payload)
            .await
    }

    // ── Unread Marks, Delete Conversation, Report Abuse ───────────────────

    /// Marks a conversation as unread (`/api/conv/addUnreadMark`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn add_unread_mark(&self, item: &ThreadMsgRef) -> Result<Value, ClientError> {
        let payload = add_unread_payload(item, &self.device.imei);
        self.post_encrypted(
            self.conversation_base(),
            "/api/conv/addUnreadMark",
            &payload,
        )
        .await
    }

    /// Clears a server unread mark (`/api/conv/removeUnreadMark`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn remove_unread_mark(
        &self,
        thread_id: &str,
        is_group: bool,
        ts: i64,
    ) -> Result<Value, ClientError> {
        let payload = remove_unread_payload(thread_id, is_group, ts);
        self.post_encrypted(
            self.conversation_base(),
            "/api/conv/removeUnreadMark",
            &payload,
        )
        .await
    }

    /// Deletes a conversation on server (`/api/message/deleteconver` or `/api/group/deleteconver`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn delete_conversation(
        &self,
        item: &ThreadMsgRef,
        client_id: u64,
    ) -> Result<Value, ClientError> {
        let payload = delete_conver_payload(item, client_id, &self.device.imei);
        if item.is_group {
            self.post_encrypted(&self.group_base(), "/api/group/deleteconver", &payload)
                .await
        } else {
            self.post_encrypted(&self.chat_base(), "/api/message/deleteconver", &payload)
                .await
        }
    }

    /// Reports a user or group (`/api/social/profile/reportabuse`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn report_abuse(
        &self,
        target_id: &str,
        is_group: bool,
        reason: i64,
        content: &str,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "uidTo": strip_group(target_id),
            "type": if is_group { REPORT_TYPE_GROUP } else { REPORT_TYPE_USER },
            "reason": reason,
            "content": content,
            "imei": self.device.imei,
        });
        self.post_encrypted(
            &self.profile_base(),
            "/api/social/profile/reportabuse",
            &payload,
        )
        .await
    }

    // ── Group Invitations Box & Group Management ──────────────────────────

    /// Lists pending group/community invitations (`/api/group/inv-box/list`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_invites(&self, page: u32, per_page: u32) -> Result<Value, ClientError> {
        let payload = json!({
            "mpage": 1,
            "page": page,
            "invPerPage": per_page,
            "mcount": 10,
            "lastGroupId": Value::Null,
            "avatar_size": 120,
            "member_avatar_size": 120,
        });
        self.get_encrypted(&self.group_base(), "/api/group/inv-box/list", &payload)
            .await
    }

    /// Accepts a group invitation from the invitation box (`/api/group/inv-box/join`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn accept_group_invite(&self, grid: &str) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "lang": "vi",
        });
        self.get_encrypted(&self.group_base(), "/api/group/inv-box/join", &payload)
            .await
    }

    /// Deletes/declines group invitations (`/api/group/inv-box/mdel-inv`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn decline_group_invites(
        &self,
        invitations: &[Value],
        block: bool,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "invitations": Value::Array(invitations.to_vec()).to_string(),
            "block": if block { 1 } else { 0 },
        });
        self.get_encrypted(&self.group_base(), "/api/group/inv-box/mdel-inv", &payload)
            .await
    }

    /// Promotes members to group co-admin (`/api/group/admins/add`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn add_group_admins(
        &self,
        grid: &str,
        members: &[String],
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "members": members,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_base(), "/api/group/admins/add", &payload)
            .await
    }

    /// Demotes group co-admins (`/api/group/admins/remove`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn remove_group_admins(
        &self,
        grid: &str,
        members: &[String],
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "members": members,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_base(), "/api/group/admins/remove", &payload)
            .await
    }

    /// Transfers group ownership (`/api/group/change-owner`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn change_group_owner(
        &self,
        grid: &str,
        new_owner_id: &str,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "newAdminId": new_owner_id,
            "imei": self.device.imei,
            "language": "vi",
        });
        self.post_encrypted(&self.group_base(), "/api/group/change-owner", &payload)
            .await
    }

    /// Lists pending join-approval members (`/api/group/pending-mems/list`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_pending_members(&self, grid: &str) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_base(), "/api/group/pending-mems/list", &payload)
            .await
    }

    /// Approves or rejects pending group join requests (`/api/group/pending-mems/review`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn review_group_pending_members(
        &self,
        grid: &str,
        members: &[String],
        approve: bool,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "members": members,
            "isApprove": if approve { 1 } else { 0 },
        });
        self.get_encrypted(
            &self.group_base(),
            "/api/group/pending-mems/review",
            &payload,
        )
        .await
    }

    /// Fetches group invite link status and URL (`/api/group/link/detail`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_link_detail(&self, grid: &str) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_base(), "/api/group/link/detail", &payload)
            .await
    }

    /// Generates / rotates the group invite link (`/api/group/link/new`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn renew_group_link(&self, grid: &str) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_base(), "/api/group/link/new", &payload)
            .await
    }

    /// Disables the group invite link (`/api/group/link/disable`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn disable_group_link(&self, grid: &str) -> Result<Value, ClientError> {
        let payload = json!({ "grid": strip_group(grid) });
        self.get_encrypted(&self.group_base(), "/api/group/link/disable", &payload)
            .await
    }

    /// Updates group permissions (`/api/group/setting/update`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_group_settings(
        &self,
        grid: &str,
        patch: &GroupSettingsPatch,
    ) -> Result<Value, ClientError> {
        let payload = group_settings_payload(grid, patch, &self.device.imei);
        self.get_encrypted(&self.group_base(), "/api/group/setting/update", &payload)
            .await
    }

    /// Disbands / disperses a group (`/api/group/disperse`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn disperse_group(&self, grid: &str) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "imei": self.device.imei,
        });
        self.post_encrypted(&self.group_base(), "/api/group/disperse", &payload)
            .await
    }

    // ── Board, Reminders, Pins & Polls ────────────────────────────────────

    /// Lists group board entries (`/api/board/list`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn list_board(
        &self,
        grid: &str,
        board_type: i64,
        page: u32,
        count: u32,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "group_id": strip_group(grid),
            "board_type": board_type,
            "page": page,
            "count": count,
            "last_id": 0,
            "last_type": 0,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_board_base(), "/api/board/list", &payload)
            .await
    }

    /// Lists 1-on-1 reminders (`/api/board/oneone/list`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn list_oneone_board(
        &self,
        uid: &str,
        board_type: i64,
        page: u32,
        count: u32,
    ) -> Result<Value, ClientError> {
        let obj = json!({
            "uid": uid,
            "board_type": board_type,
            "page": page,
            "count": count,
            "last_id": 0,
            "last_type": 0,
        });
        let payload = json!({ "objectData": obj.to_string() });
        self.get_encrypted(&self.group_board_base(), "/api/board/oneone/list", &payload)
            .await
    }

    /// Creates a group note or reminder (`/api/board/topic/createv2`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn create_group_topic(
        &self,
        grid: &str,
        topic_type: i64,
        title: &str,
        start_time: i64,
        repeat: i64,
        pin_act: i64,
    ) -> Result<Value, ClientError> {
        let payload = group_topic_payload(
            grid,
            topic_type,
            title,
            start_time,
            repeat,
            pin_act,
            &self.device.imei,
        );
        self.post_encrypted(
            &self.group_board_base(),
            "/api/board/topic/createv2",
            &payload,
        )
        .await
    }

    /// Deletes a group board topic or reminder (`/api/board/topic/remove`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn remove_group_topic(
        &self,
        grid: &str,
        topic_id: &str,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "topicId": topic_id,
            "imei": self.device.imei,
        });
        self.post_encrypted(
            &self.group_board_base(),
            "/api/board/topic/remove",
            &payload,
        )
        .await
    }

    /// Updates a group note or reminder (`/api/board/topic/updatev2`, cmd 12008).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_group_topic(
        &self,
        grid: &str,
        topic_id: &str,
        topic_type: i64,
        title: &str,
        start_time: i64,
        repeat: i64,
        pin_act: i64,
    ) -> Result<Value, ClientError> {
        let payload = group_topic_update_payload(
            grid,
            topic_id,
            topic_type,
            title,
            start_time,
            repeat,
            pin_act,
            &self.device.imei,
        );
        self.post_encrypted(
            &self.group_board_base(),
            "/api/board/topic/updatev2",
            &payload,
        )
        .await
    }

    /// Answers a group reminder (`/api/board/topic/updateEventResponse`, cmd 12087).
    ///
    /// `status` is [`EVENT_ACCEPT`] (1) or [`EVENT_REJECT`] (2).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn respond_group_event(
        &self,
        grid: &str,
        status: i64,
        event_id: &str,
    ) -> Result<Value, ClientError> {
        let payload = event_response_payload(grid, status, event_id, &self.device.imei);
        self.post_encrypted(
            &self.group_board_base(),
            "/api/board/topic/updateEventResponse",
            &payload,
        )
        .await
    }

    /// Lists who answered a group reminder (`/api/board/topic/listResponseEvent`, cmd 12086).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn list_event_responses(&self, event_id: &str) -> Result<Value, ClientError> {
        let payload = list_response_event_payload(event_id);
        self.get_encrypted(
            &self.group_board_base(),
            "/api/board/topic/listResponseEvent",
            &payload,
        )
        .await
    }

    /// Loads one group reminder (`/api/board/topic/getReminder`, cmd 12088).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_reminder_detail(&self, event_id: &str) -> Result<Value, ClientError> {
        let payload = reminder_detail_payload(event_id, &self.device.imei);
        self.get_encrypted(
            &self.group_board_base(),
            "/api/board/topic/getReminder",
            &payload,
        )
        .await
    }

    /// Creates a 1-on-1 chat reminder (`/api/board/oneone/create`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn create_oneone_reminder(
        &self,
        to_uid: &str,
        title: &str,
        start_time: i64,
        repeat: i64,
        creator_uid: &str,
    ) -> Result<Value, ClientError> {
        let payload = oneone_reminder_payload(
            to_uid,
            title,
            start_time,
            repeat,
            creator_uid,
            &self.device.imei,
        );
        self.post_encrypted(
            &self.group_board_base(),
            "/api/board/oneone/create",
            &payload,
        )
        .await
    }

    /// Removes a 1-on-1 chat reminder (`/api/board/oneone/remove`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn remove_oneone_reminder(
        &self,
        uid: &str,
        reminder_id: &str,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "uid": uid,
            "reminderId": reminder_id,
        });
        self.post_encrypted(
            &self.group_board_base(),
            "/api/board/oneone/remove",
            &payload,
        )
        .await
    }

    /// Pins a group board topic/message (`/api/board/pinv2`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn pin_board_item(
        &self,
        grid: &str,
        topic_id: &str,
        item_type: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "topicId": topic_id,
            "type": item_type,
        });
        self.get_encrypted(&self.group_board_base(), "/api/board/pinv2", &payload)
            .await
    }

    /// Unpins a group board topic/message (`/api/board/unpinv2`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn unpin_board_item(
        &self,
        grid: &str,
        topic_id: &str,
        board_version: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "topic": topic_id,
            "boardVersion": board_version,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_board_base(), "/api/board/unpinv2", &payload)
            .await
    }

    /// Creates a group poll (`/api/poll/create`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn create_poll(
        &self,
        grid: &str,
        question: &str,
        options: &[String],
        allow_multi: bool,
        allow_add: bool,
        is_anonymous: bool,
    ) -> Result<Value, ClientError> {
        let payload = create_poll_payload(
            grid,
            question,
            options,
            allow_multi,
            allow_add,
            is_anonymous,
            &self.device.imei,
        );
        self.post_encrypted(&self.group_poll_base(), "/api/poll/create", &payload)
            .await
    }

    /// Fetches poll detail (`/api/poll/detail`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_poll_detail(&self, poll_id: i64) -> Result<Value, ClientError> {
        let payload = json!({
            "poll_id": poll_id,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_poll_base(), "/api/poll/detail", &payload)
            .await
    }

    /// Submits votes for a group poll (`/api/poll/vote`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn vote_poll(&self, poll_id: i64, option_ids: &[i64]) -> Result<Value, ClientError> {
        let payload = json!({
            "poll_id": poll_id,
            "option_ids": option_ids,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.group_poll_base(), "/api/poll/vote", &payload)
            .await
    }

    /// Adds options to an existing poll (`/api/poll/option/add`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn add_poll_options(
        &self,
        poll_id: i64,
        new_options: &[String],
        voted_ids: &[i64],
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "poll_id": poll_id,
            "new_options": new_options,
            "voted_option_ids": voted_ids,
        });
        self.get_encrypted(&self.group_poll_base(), "/api/poll/option/add", &payload)
            .await
    }

    /// Closes / locks a poll (`/api/poll/end`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn end_poll(&self, poll_id: i64) -> Result<Value, ClientError> {
        let payload = json!({
            "poll_id": poll_id,
            "imei": self.device.imei,
        });
        self.post_encrypted(&self.group_poll_base(), "/api/poll/end", &payload)
            .await
    }

    // ── Personalized Stickers & Two-Factor Auth ───────────────────────────

    /// Lists user's installed/downloaded sticker category IDs (`/api/message/sticker/personalized/list`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_personalized_stickers(&self) -> Result<Value, ClientError> {
        let payload = json!({ "imei": self.device.imei });
        self.get_encrypted(
            self.sticker_base(),
            "/api/message/sticker/personalized/list",
            &payload,
        )
        .await
    }

    /// Updates user's installed/downloaded sticker pack list (`/api/message/sticker/personalized/update`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_personalized_stickers(
        &self,
        sticker_cates: &[i64],
        version: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "version": version,
            "sticker_cates": sticker_cates,
            "imei": self.device.imei,
        });
        self.get_encrypted(
            self.sticker_base(),
            "/api/message/sticker/personalized/update",
            &payload,
        )
        .await
    }

    /// Updates two-factor authentication setting (`/api/update/setting/2FA`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo (including OTP challenge) or decryption errors.
    pub async fn update_tfa_settings(
        &self,
        turn_on: bool,
        otp: Option<&str>,
    ) -> Result<Value, ClientError> {
        let mut payload = json!({
            "updateType": if turn_on { 1 } else { 2 },
            "submitOTP": if otp.is_some() { 1 } else { 0 },
            "language": "vi",
        });
        if let Some(code) = otp {
            payload["otp"] = Value::from(code);
        }
        let base = self.auth_base.clone();
        self.get_encrypted(&base, "/api/update/setting/2FA", &payload)
            .await
    }

    fn message_file_host(&self) -> String {
        self.file_host
            .clone()
            .unwrap_or_else(|| "https://tt-files-wpa.chat.zalo.me".to_owned())
    }

    fn catalog_host(&self) -> String {
        self.service_base("catalog", "https://tt-catalog-wpa.chat.zalo.me")
            .to_owned()
    }

    /// One page of product catalogs (`/api/prodcatalog/catalog/list`, cmd 12038).
    ///
    /// Electron `getAllCatalog` posts `{version_list_catalog, limit:20, last_product_id:-1, page}`
    /// to the catalog host. This does not create or delete a product.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn list_catalog_page(
        &self,
        version_list_catalog: i64,
        page: i64,
    ) -> Result<Value, ClientError> {
        let payload = catalog_list_payload(version_list_catalog, page);
        self.post_encrypted(
            &self.catalog_host(),
            "/api/prodcatalog/catalog/list",
            &payload,
        )
        .await
    }

    /// Follows `has_more` for at most ten pages and returns `(id, name)` rows.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn list_catalogs(&self) -> Result<Vec<(String, String)>, ClientError> {
        let mut page = 0_i64;
        let mut version = 0_i64;
        let mut rows = Vec::new();
        for _ in 0..10 {
            let value = self.list_catalog_page(version, page).await?;
            let parsed = catalog_page_from(&value);
            if version == 0 {
                if let Some(found) = parsed.version {
                    version = found;
                }
            }
            rows.extend(parsed.rows);
            if !parsed.has_more {
                break;
            }
            page += 1;
        }
        Ok(rows)
    }

    /// Old phone sync: snapshot lookup (`/api/message/get_crossdb`, cmd 12412).
    ///
    /// Sync 2 is the path the settings card runs. This method does not run by itself.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_crossdb(&self, sync_session: &str) -> Result<Value, ClientError> {
        let payload = crossdb_payload(&self.device.computer_name, sync_session);
        self.get_encrypted(
            &self.message_file_host(),
            "/api/message/get_crossdb",
            &payload,
        )
        .await
    }

    /// Old phone sync: ask the phone to push messages (`/api/message/pull_mobile_msg`, cmd 12000).
    ///
    /// A negative `from_seq_id` is rejected here and does not hit the network.
    /// Electron adds 1 when the sequence is positive, and sends 0 otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on a negative sequence, or on transport, Zalo or decryption errors.
    pub async fn pull_mobile_msg(
        &self,
        public_key: &str,
        from_seq_id: i64,
        is_retry: i64,
        min_seq_id: i64,
        temp_key: &str,
    ) -> Result<Value, ClientError> {
        let from_seq_id = pull_mobile_from_seq(from_seq_id)
            .map_err(|msg| ClientError::Unexpected(msg.to_owned()))?;
        let payload = pull_mobile_payload(
            &self.device.computer_name,
            public_key,
            from_seq_id,
            is_retry,
            min_seq_id,
            temp_key,
            &self.device.imei,
        );
        self.get_encrypted(
            &self.message_file_host(),
            "/api/message/pull_mobile_msg",
            &payload,
        )
        .await
    }

    /// Old phone sync: stop a pull (`/api/message/cancel_pull_mobile_msg`, cmd 12700).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn cancel_pull_mobile_msg(&self, public_key: &str) -> Result<Value, ClientError> {
        let payload =
            cancel_pull_payload(&self.device.computer_name, public_key, &self.device.imei);
        self.get_encrypted(
            &self.message_file_host(),
            "/api/message/cancel_pull_mobile_msg",
            &payload,
        )
        .await
    }

    /// Owner upgrade of a group into a community (`/api/group/upgrade/community`, cmd 12180).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn upgrade_group_to_community(
        &self,
        grid: &str,
        language: &str,
    ) -> Result<Value, ClientError> {
        let payload = community_upgrade_payload(strip_group(grid), language);
        self.get_encrypted(&self.group_base(), "/api/group/upgrade/community", &payload)
            .await
    }

    /// Community quota (`/api/group/quota/community`, cmd 12181).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_community_quota(
        &self,
        grid: Option<&str>,
        language: &str,
    ) -> Result<Value, ClientError> {
        let payload = community_quota_payload(grid.map(strip_group), language);
        self.get_encrypted(&self.group_base(), "/api/group/quota/community", &payload)
            .await
    }

    /// Conversations with a server TTL (`/api/conv/autodelete/getConvers`, cmd 12022).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_auto_delete_conversations(&self) -> Result<Value, ClientError> {
        self.get_encrypted(
            self.conversation_base(),
            "/api/conv/autodelete/getConvers",
            &json!({}),
        )
        .await
    }

    /// Sets one conversation TTL (`/api/conv/autodelete/updateConvers`, cmd 12023).
    ///
    /// The desktop UI does not call this. Electron sends `clientLang`, not `language`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_auto_delete_conversation(
        &self,
        thread_id: &str,
        is_group: bool,
        ttl_ms: i64,
        client_lang: &str,
    ) -> Result<Value, ClientError> {
        let payload =
            autodelete_update_payload(strip_group(thread_id), is_group, ttl_ms, client_lang);
        self.post_encrypted(
            self.conversation_base(),
            "/api/conv/autodelete/updateConvers",
            &payload,
        )
        .await
    }
}

/// `getAllCatalog` body. Limit and `last_product_id` match `compact3530.js`.
#[must_use]
pub fn catalog_list_payload(version_list_catalog: i64, page: i64) -> Value {
    json!({
        "version_list_catalog": version_list_catalog,
        "limit": 20,
        "last_product_id": -1,
        "page": page,
    })
}

/// One catalog page after Electron remaps `id`/`name`/`version`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogPage {
    /// `version` or `version_list_catalog` when the server sent one.
    pub version: Option<i64>,
    /// `(catalog id, display name)`.
    pub rows: Vec<(String, String)>,
    /// Server `has_more`.
    pub has_more: bool,
}

/// Reads a `getAllCatalog` reply. Accepts raw `id`/`name` and the remapped names.
#[must_use]
pub fn catalog_page_from(value: &Value) -> CatalogPage {
    let root = value.get("data").filter(|v| !v.is_null()).unwrap_or(value);
    let version = json_i64(root, "version_list_catalog").or_else(|| json_i64(root, "version"));
    let has_more = match root.get("has_more") {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    };
    let items = root
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| root.as_array());
    let rows = items
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let id = item
                        .get("catalog_id")
                        .or_else(|| item.get("id"))
                        .and_then(id_text)?;
                    let name = item
                        .get("catalog_name")
                        .or_else(|| item.get("name"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("(không tên)")
                        .to_owned();
                    Some((id, name))
                })
                .collect()
        })
        .unwrap_or_default();
    CatalogPage {
        version,
        rows,
        has_more,
    }
}

/// Bare group ids from `getE2eeGroupList`. Electron prefixes each entry; this stores digits only.
#[must_use]
pub fn e2ee_group_ids(value: &Value) -> Vec<String> {
    let arr = value
        .as_array()
        .or_else(|| value.get("data").and_then(Value::as_array))
        .or_else(|| value.get("groupIds").and_then(Value::as_array));
    let Some(arr) = arr else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        let raw = id_text(item)
            .or_else(|| item.get("grid").and_then(id_text))
            .or_else(|| item.get("groupId").and_then(id_text));
        let Some(raw) = raw else {
            continue;
        };
        let bare = bare_group_id(&raw);
        if bare.is_empty() || out.iter().any(|id| id == &bare) {
            continue;
        }
        out.push(bare);
    }
    out
}

/// True when `thread_id` (`g_…`, `g…`, or bare) is in the E2EE list.
#[must_use]
pub fn group_marked_e2ee(ids: &[String], thread_id: &str) -> bool {
    let bare = bare_group_id(thread_id);
    !bare.is_empty() && ids.iter().any(|id| id == &bare)
}

/// `getCrossDB` body.
#[must_use]
pub fn crossdb_payload(pc_name: &str, sync_session: &str) -> Value {
    json!({
        "pc_name": pc_name,
        "sync_session": sync_session,
    })
}

/// Electron `pullMobileMsg`: negative is an error, a positive sequence is incremented, else 0.
#[must_use]
pub fn pull_mobile_from_seq(from_seq_id: i64) -> Result<i64, &'static str> {
    if from_seq_id < 0 {
        Err("from_seq_id negative")
    } else if from_seq_id > 0 {
        Ok(from_seq_id.saturating_add(1))
    } else {
        Ok(0)
    }
}

/// `pullMobileMsg` body.
#[must_use]
pub fn pull_mobile_payload(
    pc_name: &str,
    public_key: &str,
    from_seq_id: i64,
    is_retry: i64,
    min_seq_id: i64,
    temp_key: &str,
    imei: &str,
) -> Value {
    json!({
        "pc_name": pc_name,
        "public_key": public_key,
        "from_seq_id": from_seq_id,
        "is_retry": is_retry,
        "min_seq_id": min_seq_id,
        "temp_key": temp_key,
        "imei": imei,
    })
}

/// `cancelTransferMessage` body.
#[must_use]
pub fn cancel_pull_payload(pc_name: &str, public_key: &str, imei: &str) -> Value {
    json!({
        "pc_name": pc_name,
        "public_key": public_key,
        "imei": imei,
    })
}

/// `upgradeGroupToCommunity` body. `grId` is the group id without `g_`.
#[must_use]
pub fn community_upgrade_payload(gr_id: &str, language: &str) -> Value {
    json!({
        "grId": gr_id,
        "language": language,
    })
}

/// `getQuotaCommunity` body. `grId` is omitted when the caller has no group.
#[must_use]
pub fn community_quota_payload(gr_id: Option<&str>, language: &str) -> Value {
    match gr_id {
        Some(id) => json!({ "language": language, "grId": id }),
        None => json!({ "language": language, "grId": Value::Null }),
    }
}

/// `updateAutoDeleteConvers` body. The field name is `clientLang`.
#[must_use]
pub fn autodelete_update_payload(
    thread_id: &str,
    is_group: bool,
    ttl_ms: i64,
    client_lang: &str,
) -> Value {
    json!({
        "threadId": thread_id,
        "isGroup": i64::from(is_group),
        "ttl": ttl_ms,
        "clientLang": client_lang,
    })
}

/// TTL in milliseconds for one thread inside a `getConvers` reply. Missing thread is `None`.
#[must_use]
pub fn autodelete_ttl_ms(value: &Value, thread_id: &str) -> Option<i64> {
    let root = value.get("data").filter(|v| !v.is_null()).unwrap_or(value);
    let convers = root.get("convers").and_then(Value::as_array)?;
    let want = bare_group_id(thread_id);
    for item in convers {
        let dest = item.get("destId").and_then(id_text).unwrap_or_default();
        if bare_group_id(&dest) != want && dest != thread_id {
            continue;
        }
        return json_i64(item, "ttl");
    }
    None
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    let v = value.get(key)?;
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn id_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        return Some(text.to_owned());
    }
    value
        .as_i64()
        .map(|n| n.to_string())
        .or_else(|| value.as_u64().map(|n| n.to_string()))
}

fn bare_group_id(id: &str) -> String {
    let id = id.trim();
    let id = id.strip_prefix("g_").unwrap_or(id);
    if let Some(rest) = id.strip_prefix('g') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return rest.to_owned();
        }
    }
    id.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_mark_payloads_match_bundle() {
        let item = ThreadMsgRef {
            thread_id: "g_99".into(),
            is_group: true,
            cli_msg_id: "123".into(),
            global_msg_id: "456".into(),
            from_uid: "789".into(),
            ts: 1_700_000_000_000,
        };
        let add = add_unread_payload(&item, "imei-1");
        let inner: Value = serde_json::from_str(add["param"].as_str().unwrap()).unwrap();
        assert_eq!(inner["convsGroup"][0]["id"], "99");
        assert_eq!(inner["convsGroup"][0]["cliMsgId"], "123");
        assert_eq!(inner["convsGroup"][0]["fromUid"], "789");
        assert_eq!(inner["imei"], "imei-1");

        let rem = remove_unread_payload("g_99", true, 1_700_000_000_111);
        let rem_inner: Value = serde_json::from_str(rem["param"].as_str().unwrap()).unwrap();
        assert_eq!(rem_inner["convsGroup"][0], "99");
        assert_eq!(rem_inner["convsGroupData"][0]["id"], "99");
        assert_eq!(rem_inner["convsGroupData"][0]["ts"], 1_700_000_000_111_i64);
    }

    #[test]
    fn delete_conver_and_settings_payloads_match_bundle() {
        let direct = ThreadMsgRef {
            thread_id: "555".into(),
            is_group: false,
            cli_msg_id: "10".into(),
            global_msg_id: "20".into(),
            from_uid: "555".into(),
            ts: 99,
        };
        let p = delete_conver_payload(&direct, 777, "imei");
        assert_eq!(p["toid"], "555");
        assert_eq!(p["cliMsgId"], "777");
        assert_eq!(p["onlyMe"], 1);
        assert_eq!(p["conver"]["globalMsgId"], "20");

        let gs = group_settings_payload(
            "g_42",
            &GroupSettingsPatch {
                lock_send_msg: Some(1),
                join_appr: Some(0),
                ..GroupSettingsPatch::default()
            },
            "imei",
        );
        assert_eq!(gs["grid"], "42");
        assert_eq!(gs["lockSendMsg"], 1);
        assert_eq!(gs["joinAppr"], 0);
        assert_eq!(gs.get("blockName"), None);
    }

    #[test]
    fn board_and_poll_payloads_match_bundle() {
        let t = group_topic_payload(
            "g_12",
            TOPIC_REMINDER,
            "Hop nhom",
            1_800_000_000_000,
            1,
            1,
            "imei",
        );
        assert_eq!(t["grid"], "12");
        assert_eq!(t["type"], TOPIC_REMINDER);
        assert_eq!(t["repeat"], 1);
        assert_eq!(t["pinAct"], 1);
        assert!(t["params"].as_str().unwrap().contains("Hop nhom"));
        assert!(t.get("topicId").is_none());
        assert!(t.get("needPin").is_none());

        let upd = group_topic_update_payload(
            "g_12",
            "t9",
            TOPIC_REMINDER,
            "Hop lai",
            1_700_000_000,
            2,
            2,
            "imei",
        );
        assert_eq!(upd["grid"], "12");
        assert_eq!(upd["topicId"], "t9");
        assert_eq!(upd["type"], TOPIC_REMINDER);
        assert_eq!(upd["startTime"], 1_700_000_000);
        assert_eq!(upd["repeat"], 2);
        assert_eq!(upd["pinAct"], 2);
        assert_eq!(upd["duration"], -1);
        assert!(upd.get("needPin").is_none());
        assert!(upd["params"].as_str().unwrap().contains("Hop lai"));

        let accept = event_response_payload("g_12", EVENT_ACCEPT, "88", "imei");
        assert_eq!(accept["grid"], "12");
        assert_eq!(accept["type"], 1);
        assert_eq!(accept["eventId"], 88);
        let reject = event_response_payload("g_12", EVENT_REJECT, "ev-x", "imei");
        assert_eq!(reject["type"], 2);
        assert_eq!(reject["eventId"], "ev-x");

        let listed = list_response_event_payload("88");
        assert_eq!(listed.as_object().unwrap().len(), 1);
        assert_eq!(listed["eventId"], 88);
        let detail = reminder_detail_payload("ev-x", "imei");
        assert_eq!(detail["eventId"], "ev-x");
        assert_eq!(detail["imei"], "imei");
        assert!(detail.get("grid").is_none());

        let o = oneone_reminder_payload("222", "Nhac viec", 1_800_000_000_000, 0, "111", "imei");
        let obj: Value = serde_json::from_str(o["objectData"].as_str().unwrap()).unwrap();
        assert_eq!(obj["toUid"], "222");
        assert_eq!(obj["creatorUid"], "111");
        assert_eq!(obj["params"]["title"], "Nhac viec");

        let poll = create_poll_payload(
            "g_12",
            "An gi?",
            &["Com".into(), "Pho".into()],
            true,
            true,
            false,
            "imei",
        );
        assert_eq!(poll["group_id"], "12");
        assert_eq!(poll["question"], "An gi?");
        assert_eq!(poll["options"][1], "Pho");
        assert_eq!(poll["allow_multi_choices"], true);
    }

    #[test]
    fn catalog_sync_community_and_e2ee_payloads_match_bundle() {
        let page = catalog_list_payload(0, 0);
        assert_eq!(page["version_list_catalog"], 0);
        assert_eq!(page["limit"], 20);
        assert_eq!(page["last_product_id"], -1);
        assert_eq!(page["page"], 0);

        let parsed = catalog_page_from(&json!({
            "version": 4,
            "has_more": 1,
            "items": [
                {"id": 9, "name": "Ao", "version": 1},
                {"catalog_id": "2", "catalog_name": "Mu"}
            ]
        }));
        assert_eq!(parsed.version, Some(4));
        assert!(parsed.has_more);
        assert_eq!(
            parsed.rows,
            vec![("9".into(), "Ao".into()), ("2".into(), "Mu".into())]
        );

        let ids = e2ee_group_ids(&json!(["12", 34, "g_12"]));
        assert_eq!(ids, vec!["12".to_owned(), "34".to_owned()]);
        assert!(group_marked_e2ee(&ids, "g_12"));
        assert!(group_marked_e2ee(&ids, "g34"));
        assert!(!group_marked_e2ee(&ids, "g_99"));

        assert_eq!(pull_mobile_from_seq(-1), Err("from_seq_id negative"));
        assert_eq!(pull_mobile_from_seq(0), Ok(0));
        assert_eq!(pull_mobile_from_seq(7), Ok(8));
        let pull = pull_mobile_payload("pc", "pk", 8, 0, 1, "tk", "imei");
        assert_eq!(pull["from_seq_id"], 8);
        assert_eq!(pull["public_key"], "pk");
        assert_eq!(pull["temp_key"], "tk");
        assert_eq!(pull["imei"], "imei");
        let cross = crossdb_payload("pc", "sess");
        assert_eq!(cross["pc_name"], "pc");
        assert_eq!(cross["sync_session"], "sess");
        let cancel = cancel_pull_payload("pc", "pk", "imei");
        assert_eq!(cancel.as_object().unwrap().len(), 3);

        let up = community_upgrade_payload("42", "vi");
        assert_eq!(up["grId"], "42");
        assert_eq!(up["language"], "vi");
        let quota = community_quota_payload(None, "en");
        assert!(quota["grId"].is_null());
        assert_eq!(quota["language"], "en");

        let ttl = autodelete_update_payload("42", true, 86_400_000, "vi");
        assert_eq!(ttl["threadId"], "42");
        assert_eq!(ttl["isGroup"], 1);
        assert_eq!(ttl["ttl"], 86_400_000);
        assert_eq!(ttl["clientLang"], "vi");
        assert!(ttl.get("language").is_none());
        let listed = json!({"convers": [
            {"destId": "g12", "isGroup": 1, "ttl": 86_400_000},
            {"destId": "99", "isGroup": 0, "ttl": 0}
        ]});
        assert_eq!(autodelete_ttl_ms(&listed, "g_12"), Some(86_400_000));
        assert_eq!(autodelete_ttl_ms(&listed, "77"), None);
    }
}
