//! Chat actions taken from Zalo PC `compact3178.js` / `compact2416.js`.
//!
//! Payload builders are tested without the network. The `ZaloApi` methods
//! encrypt those payloads the same way as [`send_message`](ZaloApi::send_message).

use super::api::{ClientError, ZaloApi};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

/// `MSG_TEXT` in module `1pet` (`compact0293.js`).
pub const MSG_TEXT: i64 = 1;
/// `MSG_VOICE` (`chat.voice`) in the same enum (`V = 3`).
pub const MSG_VOICE: i64 = 3;
/// `MSG_FILE` (`share.file`) in the same enum.
pub const MSG_FILE: i64 = 19;
/// Forward `msgType` for a video (`MSGTYPE_FORWARD_VIDEO` in the PC bundle).
/// This is not `CLI_MSG_TYPE_VIDEO` (44).
pub const MSGTYPE_FORWARD_VIDEO: i64 = 5;
/// CDN that turns AMR into MP3 (`compact1847.js` `voice.voiceUrl`, player in `compact3576.js`).
pub const VOICE_MP3_HOST: &str = "https://cache.voice.talk.zdn.vn";
/// `updateListArchivedChat` actionType: add to Other (`O = 0`).
pub const ARCHIVE_ADD: i64 = 0;
/// `updateListArchivedChat` actionType: move back to Focused (`C = 1`).
pub const ARCHIVE_REMOVE: i64 = 1;
/// Cap for `sharefile_full` (one POST, 60 s in Zalo PC). Larger files use asyncfile.
pub const SHAREFILE_MAX_BYTES: u64 = 100 * 1024 * 1024;
/// `asyncfile/upload` and `video/file/upload` chunk size on Zalo PC.
pub const ASYNC_CHUNK_BYTES: u64 = 3 * 1024 * 1024;

/// How many 3 MiB chunks an async upload needs. Zero for an empty file.
#[must_use]
pub fn async_chunk_count(total: u64) -> u64 {
    if total == 0 {
        0
    } else {
        total.div_ceil(ASYNC_CHUNK_BYTES)
    }
}
/// `updatePinnedConversationsV2` actionType: pin (`compact2416.js` `u`).
pub const PIN_ADD: i64 = 1;
/// `updatePinnedConversationsV2` actionType: unpin (`compact2416.js` `A`).
pub const PIN_REMOVE: i64 = 2;
/// `SET_MUTE` in `compact0293.js`.
pub const SET_MUTE: i64 = 1;
/// `UNSET_MUTE` in `compact0293.js`.
pub const UNSET_MUTE: i64 = 3;
/// `updateProfileAvatar` / `updateAvatar` always send `avatarSize` 160.
pub const AVATAR_SIZE: u32 = 160;
/// `update_avatar.min_width` / `min_height` in the PC bundle (also the
/// fallback `originWidth` / `originHeight` when the file has no size).
pub const AVATAR_MIN_EDGE: u32 = 240;
/// `update_avatar.min_size_kb`.
pub const AVATAR_MIN_BYTES: u64 = 7 * 1024;
/// `update_avatar.max_local_size` (megabytes) before the PC resizes.
pub const AVATAR_MAX_BYTES: u64 = 30 * 1024 * 1024;
/// `getOnlinStatus` `conv_type` for a group (`enable_group` is off in the
/// default config, so the UI does not call this).
pub const CONV_TYPE_GROUP: i32 = 0;
/// `getOnlinStatus` `conv_type` for a person.
pub const CONV_TYPE_USER: i32 = 1;
/// `getOnlinStatus` `conv_type` for an OA.
pub const CONV_TYPE_OA: i32 = 2;
/// Group avatar upload `error_code` when the member cannot change it.
pub const AVATAR_ERR_NO_PERMISSION: i64 = 166;
/// Avatar upload `error_code` when the file is over the server limit.
pub const AVATAR_ERR_OVERSIZE: i64 = 200;
/// Default like (`reactionMsgInfo` `rType` 3).
pub const REACT_LIKE: i64 = 3;
/// Icon string for [`REACT_LIKE`].
pub const REACT_LIKE_ICON: &str = "/-strong";

/// Message being quoted (`sendQuoteMessage` / `sendGroupQuoteMessage`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuoteTarget {
    /// Sender of the quoted message (`qmsgOwner` / `fromUid`).
    pub from_uid: String,
    /// Server id (`qmsgId`).
    pub msg_id: i64,
    /// Client id (`qmsgCliId`).
    pub cli_msg_id: i64,
    /// `cliMsgType`; [`MSG_TEXT`] for a text bubble.
    pub cli_msg_type: i64,
    /// Quoted message time, milliseconds (`qmsgTs`).
    pub ts: i64,
    /// Quoted body (`qmsg`).
    pub qmsg: String,
}

/// One row of `msgs` in `deleteOneOneMessageV2` / `deleteGroupMessageV2`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteItem {
    /// `cliMsgId`.
    pub cli_msg_id: i64,
    /// `globalMsgId`.
    pub global_msg_id: i64,
    /// Owner of the message (`ownerId`).
    pub owner_id: String,
    /// Destination user/group without the `g_` prefix, when known.
    pub dest_id: Option<String>,
}

/// One reaction (`sendReaction` → `react_list` item).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaction {
    /// `rType` (3 = like).
    pub r_type: i64,
    /// `rIcon` (`/-strong` for like).
    pub r_icon: String,
}

impl Reaction {
    /// The default like used by Zalo PC.
    #[must_use]
    pub fn like() -> Self {
        Self {
            r_type: REACT_LIKE,
            r_icon: REACT_LIKE_ICON.to_owned(),
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn strip_group(id: &str) -> &str {
    id.strip_prefix("g_").unwrap_or(id)
}

fn pin_id(thread_id: &str) -> String {
    if let Some(rest) = thread_id.strip_prefix("g_") {
        format!("g{rest}")
    } else if thread_id.starts_with('g') || thread_id.starts_with('u') {
        thread_id.to_owned()
    } else {
        format!("u{thread_id}")
    }
}

/// Conversation id as Zalo PC stores it on labels / archived chat (`g123`, not `g_123`).
#[must_use]
pub fn conv_id(thread_id: &str) -> String {
    thread_id
        .strip_prefix("g_")
        .map(|id| format!("g{id}"))
        .unwrap_or_else(|| thread_id.to_owned())
}

/// `sendFile` params (`/api/message/sharefile_full` or group). `clientId` is a string.
#[must_use]
pub fn sharefile_payload(thread_id: &str, is_group: bool, client_id: u64) -> Value {
    let target = strip_group(thread_id);
    if is_group {
        json!({ "grid": target, "clientId": client_id.to_string() })
    } else {
        json!({ "toid": target, "clientId": client_id.to_string() })
    }
}

/// `sendVoice` params (`/api/message/voicemp3` or `/api/group/sendmp3`).
#[must_use]
pub fn voice_payload(thread_id: &str, is_group: bool, client_id: u64) -> Value {
    sharefile_payload(thread_id, is_group, client_id)
}

/// True when the voice href is AMR (`compact3576.js` plays those via `/mp3?url=`).
#[must_use]
pub fn voice_href_is_amr(href: &str) -> bool {
    href.split('?')
        .next()
        .and_then(|path| path.rsplit('.').next())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("amr"))
}

fn encode_uri_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Play URL from `compact3576.js`: AAC/MP3 `href` as-is; AMR through `voiceUrl/mp3?url=`.
#[must_use]
pub fn voice_play_href(href: &str, msg_id: Option<i64>) -> String {
    if href.is_empty() || !voice_href_is_amr(href) {
        return href.to_owned();
    }
    let mut url = format!("{VOICE_MP3_HOST}/mp3?url={}", encode_uri_component(href));
    if let Some(id) = msg_id {
        url.push_str("&msgid=");
        url.push_str(&id.to_string());
    }
    url
}

/// `getGroupInfoV2` params (`/api/group/getgi/v2`).
#[must_use]
pub fn group_info_payload(grid: &str, page: i64, imei: &str) -> Value {
    json!({
        "grid": strip_group(grid),
        "avatar_size": 240,
        "member_avatar_size": 240,
        "mpage": page,
        "mcount": 100,
        "imei": imei,
    })
}

/// Quote payload for 1-1 (`/api/message/quote`).
#[must_use]
pub fn quote_direct_payload(
    to_uid: &str,
    message: &str,
    client_id: u64,
    imei: &str,
    q: &QuoteTarget,
) -> Value {
    json!({
        "toid": to_uid,
        "message": message,
        "clientId": client_id,
        "qmsgOwner": q.from_uid,
        "qmsgId": q.msg_id,
        "qmsgCliId": q.cli_msg_id,
        "qmsgType": q.cli_msg_type,
        "qmsgTs": q.ts,
        "qmsg": q.qmsg,
        "imei": imei,
    })
}

/// Quote payload for a group (`/api/group/quote`).
#[must_use]
pub fn quote_group_payload(grid: &str, message: &str, client_id: u64, q: &QuoteTarget) -> Value {
    json!({
        "grid": strip_group(grid),
        "message": message,
        "clientId": client_id,
        "language": "vi",
        "qmsgOwner": q.from_uid,
        "qmsgId": q.msg_id,
        "qmsgCliId": q.cli_msg_id,
        "qmsgType": q.cli_msg_type,
        "qmsg": q.qmsg,
        "qmsgTs": q.ts,
        "visibility": 0,
    })
}

/// Undo 1-1 (`/api/message/undo`).
#[must_use]
pub fn undo_direct_payload(
    to_uid: &str,
    msg_id: i64,
    client_id: u64,
    cli_msg_id_undo: i64,
) -> Value {
    json!({
        "msgId": msg_id,
        "toid": to_uid,
        "clientId": client_id,
        "cliMsgIdUndo": cli_msg_id_undo,
    })
}

/// Undo group (`/api/group/undomsg`).
#[must_use]
pub fn undo_group_payload(
    grid: &str,
    msg_id: i64,
    client_id: u64,
    cli_msg_id_undo: i64,
    imei: &str,
) -> Value {
    json!({
        "msgId": msg_id,
        "grid": strip_group(grid),
        "clientId": client_id,
        "visibility": 0,
        "cliMsgIdUndo": cli_msg_id_undo,
        "imei": imei,
    })
}

fn delete_msgs(items: &[DeleteItem]) -> Vec<Value> {
    items
        .iter()
        .map(|item| {
            // Zalo PC `u3IG` / `syncDeleteMessage` sends all four `msgs[]` fields
            // as strings and strips the group prefix from `destId`.
            let mut row = json!({
                "cliMsgId": item.cli_msg_id.to_string(),
                "globalMsgId": item.global_msg_id.to_string(),
                "ownerId": item.owner_id,
            });
            if let Some(dest) = &item.dest_id {
                let clean = strip_group(dest);
                if !clean.is_empty() {
                    row["destId"] = Value::from(clean);
                }
            }
            row
        })
        .collect()
}

/// Delete 1-1 (`/api/message/delete`). `only_me` 1 = this device, 0 = everyone.
#[must_use]
pub fn delete_direct_payload(
    to_uid: &str,
    cli_msg_id: i64,
    items: &[DeleteItem],
    only_me: bool,
    imei: &str,
) -> Value {
    json!({
        "toid": to_uid,
        "cliMsgId": cli_msg_id,
        "msgs": delete_msgs(items),
        "onlyMe": if only_me { 1 } else { 0 },
        "imei": imei,
    })
}

/// Delete group (`/api/group/deletemsg`).
#[must_use]
pub fn delete_group_payload(
    grid: &str,
    cli_msg_id: i64,
    items: &[DeleteItem],
    only_me: bool,
) -> Value {
    json!({
        "grid": strip_group(grid),
        "cliMsgId": cli_msg_id,
        "msgs": delete_msgs(items),
        "onlyMe": if only_me { 1 } else { 0 },
    })
}

/// Text forward 1-1 (`/api/message/forward`).
#[must_use]
pub fn forward_direct_payload(to_uid: &str, text: &str, client_id: u64, imei: &str) -> Value {
    json!({
        "toId": to_uid,
        "imei": imei,
        "msgType": MSG_TEXT,
        "clientId": client_id,
        "msgInfo": json!({ "message": text }).to_string(),
    })
}

/// Text forward group (`/api/group/forward`).
#[must_use]
pub fn forward_group_payload(grid: &str, text: &str, client_id: u64) -> Value {
    json!({
        "grid": strip_group(grid),
        "visibility": 0,
        "msgType": MSG_TEXT,
        "clientId": client_id,
        "msgInfo": json!({ "message": text }).to_string(),
    })
}

/// Fields of a composed video after `video_done` (`sendMsg` case `MSG_VIDEO`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoSendInfo {
    /// File name (`title`).
    pub title: String,
    /// URL from the `video_done` control (`videoUrl`).
    pub video_url: String,
    /// JPEG thumb URL from `video/thumb/upload`. Empty when there is none.
    pub thumb_url: String,
    /// Length in milliseconds. `0` is sent as `1`, matching `duration || 1`.
    pub duration_ms: u64,
    /// Byte size (`fileSize`).
    pub file_size: u64,
    /// `video_width`. `0` omits `width`.
    pub width: u32,
    /// `video_height`. `0` omits `height`.
    pub height: u32,
}

/// Video forward (`/api/message/forward` or `/api/group/forward`, `msgType` 5).
///
/// Group `visibility` is `0` when the group cache has no value, which is what
/// `getTypeVisibility` returns in that case. The file host (not the chat host)
/// serves this call.
#[must_use]
pub fn forward_video_payload(
    thread_id: &str,
    is_group: bool,
    client_id: u64,
    imei: &str,
    info: &VideoSendInfo,
) -> Value {
    let mut msg = serde_json::Map::new();
    msg.insert("title".to_owned(), json!(info.title));
    msg.insert("videoUrl".to_owned(), json!(info.video_url));
    if !info.thumb_url.is_empty() {
        msg.insert("thumbUrl".to_owned(), json!(info.thumb_url));
    }
    let duration = if info.duration_ms == 0 {
        1
    } else {
        info.duration_ms
    };
    msg.insert("duration".to_owned(), json!(duration));
    msg.insert("properties".to_owned(), json!("{}"));
    msg.insert("fileSize".to_owned(), json!(info.file_size));
    if info.width > 0 {
        msg.insert("width".to_owned(), json!(info.width));
    }
    if info.height > 0 {
        msg.insert("height".to_owned(), json!(info.height));
    }
    let msg_info = Value::Object(msg).to_string();
    let target = strip_group(thread_id);
    if is_group {
        json!({
            "grid": target,
            "visibility": 0,
            "msgType": MSGTYPE_FORWARD_VIDEO,
            "clientId": client_id,
            "msgInfo": msg_info,
        })
    } else {
        json!({
            "toId": target,
            "imei": imei,
            "msgType": MSGTYPE_FORWARD_VIDEO,
            "clientId": client_id,
            "msgInfo": msg_info,
        })
    }
}

/// Reaction list item (`sendReaction` in `compact2514.js`).
#[must_use]
pub fn reaction_item(
    msg_id: i64,
    cli_msg_id: i64,
    msg_type: i64,
    react: &Reaction,
    client_id: u64,
) -> Value {
    let body = json!({
        "rMsg": [{ "gMsgID": msg_id, "cMsgID": cli_msg_id, "msgType": msg_type }],
        "rIcon": react.r_icon,
        "rType": react.r_type,
        "source": 6,
    });
    json!({ "message": body.to_string(), "clientId": client_id })
}

/// Contact / namecard 1-1 (`/api/message/contact`, `compact3178.js` `forwardContactOA`).
#[must_use]
pub fn contact_direct_payload(
    to_uid: &str,
    contact_uid: &str,
    client_id: u64,
    imei: &str,
) -> Value {
    json!({
        "toId": to_uid,
        "contactId": contact_uid,
        "clientId": client_id,
        "imei": imei,
    })
}

/// Contact / namecard group (`/api/group/contact`, `compact3178.js` `forwardContactOA`).
#[must_use]
pub fn contact_group_payload(grid: &str, contact_uid: &str, client_id: u64, imei: &str) -> Value {
    json!({
        "grid": strip_group(grid),
        "visibility": 0,
        "contactId": contact_uid,
        "clientId": client_id,
        "imei": imei,
    })
}

/// Text message payload with optional `metaData.urgency` (`0` = normal, `1` = important, `2` = urgent).
#[must_use]
pub fn text_urgency_payload(
    thread_id: &str,
    is_group: bool,
    message: &str,
    client_id: u64,
    imei: &str,
    urgency: u8,
) -> Value {
    let mut payload = if is_group {
        json!({
            "message": message,
            "clientId": client_id,
            "imei": imei,
            "grid": strip_group(thread_id),
            "visibility": 0,
        })
    } else {
        json!({
            "message": message,
            "clientId": client_id,
            "imei": imei,
            "toid": thread_id,
        })
    };
    if urgency > 0 {
        payload["metaData"] = json!({ "urgency": urgency });
    }
    payload
}

/// `updateProfileAvatar`: `avatarSize`, `clientId`, `language`, and
/// `metaData` (a JSON string) when the picture's edges are known.
#[must_use]
pub fn profile_avatar_payload(client_id: u64, width: u32, height: u32, size: u64) -> Value {
    let mut payload = json!({
        "avatarSize": AVATAR_SIZE,
        "clientId": client_id,
        "language": "vi",
    });
    if width > 0 && height > 0 {
        let meta = json!({
            "origin": { "width": width, "height": height },
            "processed": { "width": width, "height": height, "size": size },
        });
        payload["metaData"] = Value::String(meta.to_string());
    }
    payload
}

/// `updateAvatar`: `grid` without `g_`, `avatarSize`, `clientId`,
/// `originWidth`, `originHeight`, `imei`. Missing edges become
/// [`AVATAR_MIN_EDGE`].
#[must_use]
pub fn group_avatar_payload(
    thread_id: &str,
    client_id: u64,
    width: u32,
    height: u32,
    imei: &str,
) -> Value {
    let width = if width == 0 { AVATAR_MIN_EDGE } else { width };
    let height = if height == 0 { AVATAR_MIN_EDGE } else { height };
    json!({
        "grid": strip_group(thread_id),
        "avatarSize": AVATAR_SIZE,
        "clientId": client_id,
        "originWidth": width,
        "originHeight": height,
        "imei": imei,
    })
}

/// Rejects an empty file, anything under 7 KB, and anything over 30 MB.
pub(crate) fn avatar_len_error(len: usize) -> Option<&'static str> {
    let len = len as u64;
    if len == 0 {
        Some("empty avatar")
    } else if len < AVATAR_MIN_BYTES {
        Some("avatar smaller than 7KB")
    } else if len > AVATAR_MAX_BYTES {
        Some("avatar larger than 30MB")
    } else {
        None
    }
}

/// One row of `requestGetFriendOnlines` (`onlines[].userId` means online).
#[must_use]
pub fn friend_online_ids(value: &Value) -> Vec<String> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let Some(rows) = root.get("onlines").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in rows {
        let id = match row.get("userId") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        if id.is_empty() || id == "0" || out.contains(&id) {
            continue;
        }
        out.push(id);
    }
    out
}

/// `getOnlinStatus` reply. `hidden` when `settings.show_online_status` is off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastOnline {
    /// `lastOnline` in unix milliseconds. `0` when missing or hidden.
    pub at_ms: i64,
    /// The peer turned presence off.
    pub hidden: bool,
}

/// Reads `lastOnline` and `settings.show_online_status`.
#[must_use]
pub fn last_online_from(value: &Value) -> LastOnline {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let hidden = match root
        .get("settings")
        .and_then(|s| s.get("show_online_status"))
    {
        Some(Value::Bool(on)) => !on,
        Some(Value::Number(n)) => n.as_i64() == Some(0),
        _ => false,
    };
    if hidden {
        return LastOnline {
            at_ms: 0,
            hidden: true,
        };
    }
    let at_ms = match root.get("lastOnline") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    };
    LastOnline {
        at_ms,
        hidden: false,
    }
}

/// https URL from an avatar upload reply, when the server sent one.
#[must_use]
pub fn avatar_url_from(value: &Value) -> String {
    let mut nodes = vec![value];
    if let Some(data) = value.get("data") {
        nodes.push(data);
    }
    for node in nodes {
        for key in ["avatar", "org", "normalUrl", "hdUrl", "url", "thumbUrl"] {
            if let Some(url) = node.get(key).and_then(Value::as_str) {
                if url.starts_with("https://") || url.starts_with("http://") {
                    return url.to_owned();
                }
            }
        }
    }
    String::new()
}

/// `getMediaFromConversation` type for photos and videos (`"media"`).
pub const MEDIA_TYPE_IMAGE: &str = "media";
/// `getMediaFromConversation` / `removeMediaFromGroup` type for files.
pub const MEDIA_TYPE_FILE: &str = "file";
/// `getMediaFromConversation` / `removeMediaFromGroup` type for links.
pub const MEDIA_TYPE_LINK: &str = "link";
/// `getMediaStorePreview` default `limit`.
pub const MEDIA_PREVIEW_LIMIT: u32 = 50;
/// `removeMediaFromGroup` when the row is already gone. The PC still drops it.
pub const MEDIA_ERR_GONE: i64 = 212;
/// Compile-time host (`apiMediaStoreDomain`, domain `zaloapp.com`).
pub const MEDIA_STORE_FALLBACK: &str = "https://media-wpa.zaloapp.com";
/// Compile-time host (`apiSendToMeMediaStoreDomain`).
pub const MEDIA_STORE_SEND2ME_FALLBACK: &str = "https://mediasend2me-wpa.api.zaloapp.com";

/// One row of the media store (`_mapMediaItemProperties`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaItem {
    /// Server media id (`remove` `media_id`).
    pub id: String,
    /// `image`, `file` or `link`. Photos and videos are both `image`.
    pub kind: String,
    /// `1` photo, `2` video, `0` otherwise.
    pub sub_type: i64,
    /// `global_msg_id`.
    pub msg_id: String,
    /// `client_msg_id`.
    pub cli_msg_id: String,
    /// `file_name`, else `title`.
    pub title: String,
    /// `src_url`.
    pub url: String,
    /// `thumb_url`, else `previewThumb`.
    pub thumb: String,
    /// `file_size` bytes.
    pub file_size: u64,
    /// `width` of a photo or video.
    pub width: u32,
    /// `height` of a photo or video.
    pub height: u32,
}

/// One page of `/api/mediastore/list` or `/api/media/oneone/list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPage {
    /// Rows in this page.
    pub items: Vec<MediaItem>,
    /// `has_more` from the list reply.
    pub has_more: bool,
    /// Pass back as `last_id` for the next page.
    pub last_id: String,
}

/// `/api/mediastore/preview` split the way the info panel does.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MediaPreview {
    /// `type` `media` (photos and videos).
    pub images: Vec<MediaItem>,
    /// `type` `file`.
    pub files: Vec<MediaItem>,
    /// `type` `link`.
    pub links: Vec<MediaItem>,
}

/// Encrypted `params` for `getMediaFromConversation`.
/// A group (and a 1-1 id, which the PC also sends here) carries `group_id`
/// and `imei`. My Cloud does not.
pub fn media_list_payload(
    send2me: bool,
    thread_id: &str,
    media_type: &str,
    limit: Option<u32>,
    last_id: Option<&str>,
    last_fetch_id: Option<u64>,
    imei: &str,
) -> Value {
    let mut payload = json!({ "media_type": media_type });
    if let Some(id) = last_fetch_id {
        payload["lastFetchId"] = json!(id);
    }
    if !send2me {
        payload["group_id"] = json!(strip_group(thread_id));
        payload["imei"] = json!(imei);
    }
    if let Some(limit) = limit.filter(|n| *n > 0) {
        payload["limit"] = json!(limit);
    }
    if let Some(last) = last_id.filter(|s| !s.is_empty()) {
        payload["last_id"] = json!(last);
    }
    payload
}

/// Encrypted `params` for `getMediaStorePreview` (`limit` defaults to 50).
pub fn media_preview_payload(send2me: bool, thread_id: &str, limit: u32, imei: &str) -> Value {
    let mut payload = json!({ "limit": if limit == 0 { MEDIA_PREVIEW_LIMIT } else { limit } });
    if !send2me {
        payload["group_id"] = json!(strip_group(thread_id));
        payload["imei"] = json!(imei);
    }
    payload
}

/// Encrypted `params` for `removeMediaFromGroup`. No `imei` inside `params`.
pub fn media_remove_payload(
    send2me: bool,
    thread_id: &str,
    media_type: &str,
    media_id: &str,
) -> Value {
    let mut payload = json!({ "media_type": media_type, "media_id": media_id });
    if !send2me {
        payload["group_id"] = json!(strip_group(thread_id));
    }
    payload
}

/// List reply: `{ data: { items, has_more, last_id } }`.
pub fn media_page_from(value: &Value) -> MediaPage {
    let data = value.get("data").unwrap_or(value);
    MediaPage {
        items: item_nodes(value).map(map_items).unwrap_or_default(),
        has_more: json_flag(data, "has_more") || json_flag(value, "has_more"),
        last_id: {
            let id = json_text(data, "last_id");
            if id.is_empty() {
                json_text(value, "last_id")
            } else {
                id
            }
        },
    }
}

/// Preview reply: `{ data: [ items ] }`, split into image / file / link.
pub fn media_preview_from(value: &Value) -> MediaPreview {
    let mut out = MediaPreview::default();
    for item in item_nodes(value).map(map_items).unwrap_or_default() {
        match item.kind.as_str() {
            "image" | "media" => out.images.push(item),
            "file" => out.files.push(item),
            "link" => out.links.push(item),
            _ => {}
        }
    }
    out
}

fn item_nodes(value: &Value) -> Option<&Vec<Value>> {
    if let Some(data) = value.get("data") {
        if let Some(items) = data.get("items").and_then(Value::as_array) {
            return Some(items);
        }
        if let Some(items) = data.as_array() {
            return Some(items);
        }
    }
    value.get("items").and_then(Value::as_array)
}

fn map_items(nodes: &Vec<Value>) -> Vec<MediaItem> {
    nodes.iter().filter_map(map_item).collect()
}

fn map_item(value: &Value) -> Option<MediaItem> {
    let raw = json_text(value, "type");
    if raw.is_empty() {
        return None;
    }
    let kind = if raw == "media" {
        "image".to_owned()
    } else {
        raw
    };
    let title = {
        let name = json_text(value, "file_name");
        if name.is_empty() {
            json_text(value, "title")
        } else {
            name
        }
    };
    let thumb = {
        let thumb = json_text(value, "thumb_url");
        if thumb.is_empty() {
            json_text(value, "previewThumb")
        } else {
            thumb
        }
    };
    Some(MediaItem {
        id: json_text(value, "id"),
        kind,
        sub_type: json_i64(value, "sub_type"),
        msg_id: json_text(value, "global_msg_id"),
        cli_msg_id: json_text(value, "client_msg_id"),
        title,
        url: json_text(value, "src_url"),
        thumb,
        file_size: json_u64(value, "file_size"),
        width: json_u32(value, "width"),
        height: json_u32(value, "height"),
    })
}

fn json_text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(num)) => num.to_string(),
        _ => String::new(),
    }
}

fn json_u64(value: &Value, key: &str) -> u64 {
    match value.get(key) {
        Some(Value::Number(num)) => num
            .as_u64()
            .or_else(|| num.as_f64().map(|n| n as u64))
            .unwrap_or(0),
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn json_i64(value: &Value, key: &str) -> i64 {
    match value.get(key) {
        Some(Value::Number(num)) => num
            .as_i64()
            .or_else(|| num.as_f64().map(|n| n as i64))
            .unwrap_or(0),
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn json_u32(value: &Value, key: &str) -> u32 {
    json_u64(value, key).min(u64::from(u32::MAX)) as u32
}

fn json_flag(value: &Value, key: &str) -> bool {
    match value.get(key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(num)) => num.as_i64().unwrap_or(0) != 0,
        Some(Value::String(text)) => text == "1" || text.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

impl ZaloApi {
    pub(crate) fn chat_base(&self) -> String {
        self.chat_host
            .clone()
            .unwrap_or_else(|| "https://tt-chat2-wpa.chat.zalo.me".to_owned())
    }

    pub(crate) fn group_base(&self) -> String {
        self.group_host
            .clone()
            .unwrap_or_else(|| "https://tt-group-wpa.chat.zalo.me".to_owned())
    }

    pub(crate) fn profile_base(&self) -> String {
        self.profile_host
            .clone()
            .unwrap_or_else(|| "https://tt-profile-wpa.chat.zalo.me".to_owned())
    }

    /// Friends who are online right now (`/api/social/friend/onlines`).
    /// A uid in the list is online; the server already drops hidden peers.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn friend_onlines(&self) -> Result<Vec<String>, ClientError> {
        let payload = json!({ "imei": self.device.imei });
        let host = self.profile_base();
        let value = self
            .get_encrypted(&host, "/api/social/friend/onlines", &payload)
            .await?;
        Ok(friend_online_ids(&value))
    }

    /// Last visit of one person (`/api/social/profile/lastOnline`).
    /// `conv_type` is [`CONV_TYPE_USER`], [`CONV_TYPE_GROUP`] or [`CONV_TYPE_OA`].
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn last_online(&self, uid: &str, conv_type: i32) -> Result<LastOnline, ClientError> {
        let payload = json!({
            "uid": uid,
            "conv_type": conv_type,
            "imei": self.device.imei,
        });
        let host = self.profile_base();
        let value = self
            .get_encrypted(&host, "/api/social/profile/lastOnline", &payload)
            .await?;
        Ok(last_online_from(&value))
    }

    fn label_base(&self) -> String {
        self.label_host
            .clone()
            .unwrap_or_else(|| "https://tt-label-wpa.chat.zalo.me".to_owned())
    }

    fn reaction_base(&self) -> String {
        self.service_base("reaction", "https://tt-reaction-wpa.chat.zalo.me")
            .to_owned()
    }

    /// Sends a quote (`/api/message/quote` or `/api/group/quote`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_quote(
        &self,
        thread_id: &str,
        is_group: bool,
        message: &str,
        client_id: u64,
        quote: &QuoteTarget,
    ) -> Result<Value, ClientError> {
        if is_group {
            let payload = quote_group_payload(thread_id, message, client_id, quote);
            self.post_encrypted(&self.group_base(), "/api/group/quote", &payload)
                .await
        } else {
            let payload =
                quote_direct_payload(thread_id, message, client_id, &self.device.imei, quote);
            self.post_encrypted(&self.chat_base(), "/api/message/quote", &payload)
                .await
        }
    }

    /// Recalls a message we sent (`/api/message/undo` or `/api/group/undomsg`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn undo_message(
        &self,
        thread_id: &str,
        is_group: bool,
        msg_id: i64,
        client_id: u64,
        cli_msg_id_undo: i64,
    ) -> Result<Value, ClientError> {
        if is_group {
            let payload = undo_group_payload(
                thread_id,
                msg_id,
                client_id,
                cli_msg_id_undo,
                &self.device.imei,
            );
            self.post_encrypted(&self.group_base(), "/api/group/undomsg", &payload)
                .await
        } else {
            let payload = undo_direct_payload(thread_id, msg_id, client_id, cli_msg_id_undo);
            self.post_encrypted(&self.chat_base(), "/api/message/undo", &payload)
                .await
        }
    }

    /// Deletes messages (`/api/message/delete` or `/api/group/deletemsg`).
    ///
    /// `only_me` matches Zalo PC `onlyMe`: true = this side, false = everyone.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn delete_messages(
        &self,
        thread_id: &str,
        is_group: bool,
        cli_msg_id: i64,
        items: &[DeleteItem],
        only_me: bool,
    ) -> Result<Value, ClientError> {
        if is_group {
            let payload = delete_group_payload(thread_id, cli_msg_id, items, only_me);
            self.post_encrypted(&self.group_base(), "/api/group/deletemsg", &payload)
                .await
        } else {
            let payload =
                delete_direct_payload(thread_id, cli_msg_id, items, only_me, &self.device.imei);
            self.post_encrypted(&self.chat_base(), "/api/message/delete", &payload)
                .await
        }
    }

    /// Forwards text (`/api/message/forward` or `/api/group/forward`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn forward_text(
        &self,
        dest_id: &str,
        is_group: bool,
        text: &str,
        client_id: u64,
    ) -> Result<Value, ClientError> {
        if is_group {
            let payload = forward_group_payload(dest_id, text, client_id);
            self.post_encrypted(&self.group_base(), "/api/group/forward", &payload)
                .await
        } else {
            let payload = forward_direct_payload(dest_id, text, client_id, &self.device.imei);
            self.post_encrypted(&self.chat_base(), "/api/message/forward", &payload)
                .await
        }
    }

    /// Sends a reaction (`/api/message/reaction` or `/api/group/reaction`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_reaction(
        &self,
        thread_id: &str,
        is_group: bool,
        msg_id: i64,
        cli_msg_id: i64,
        msg_type: i64,
        react: &Reaction,
        client_id: u64,
    ) -> Result<Value, ClientError> {
        let item = reaction_item(msg_id, cli_msg_id, msg_type, react, client_id);
        let mut payload = json!({ "react_list": [item] });
        let host = self.reaction_base();
        if is_group {
            payload["grid"] = Value::from(strip_group(thread_id));
            payload["imei"] = Value::from(self.device.imei.as_str());
            self.post_encrypted(&host, "/api/group/reaction", &payload)
                .await
        } else {
            payload["toid"] = Value::from(thread_id);
            self.post_encrypted(&host, "/api/message/reaction", &payload)
                .await
        }
    }

    /// Typing indicator (`/api/message/typing` or `/api/group/typing`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_typing(&self, thread_id: &str, is_group: bool) -> Result<Value, ClientError> {
        if is_group {
            let payload = json!({
                "grid": strip_group(thread_id),
                "imei": self.device.imei,
            });
            self.post_encrypted(&self.group_base(), "/api/group/typing", &payload)
                .await
        } else {
            let payload = json!({
                "toid": thread_id,
                "destType": 1,
                "imei": self.device.imei,
            });
            self.post_encrypted(&self.chat_base(), "/api/message/typing", &payload)
                .await
        }
    }

    /// Delivered ack (`/api/message/deliveredv2` or `/api/group/deliveredv2`).
    ///
    /// `items` are the same `{cmi, gmi, si, di, mt, st, at, cmd, ts}` records
    /// as `seenv2`. The body is `{seen: 0, data}` and, for a group, `grid`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_delivered(
        &self,
        thread_id: &str,
        items: &[Value],
    ) -> Result<Value, ClientError> {
        let group = thread_id.starts_with("g_");
        let (host, path, info) = if group {
            (
                self.group_base(),
                "/api/group/deliveredv2",
                json!({
                    "seen": 0,
                    "data": items,
                    "grid": thread_id.trim_start_matches("g_"),
                }),
            )
        } else {
            (
                self.chat_base(),
                "/api/message/deliveredv2",
                json!({ "seen": 0, "data": items }),
            )
        };
        let mut payload = json!({ "msgInfos": info.to_string() });
        if group {
            payload["imei"] = Value::from(self.device.imei.as_str());
        }
        self.post_encrypted(&host, path, &payload).await
    }

    /// Pinned conversations (`/api/pinconvers/list`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_pinned_conversations(&self) -> Result<Value, ClientError> {
        let payload = json!({ "imei": self.device.imei });
        self.get_encrypted(self.conversation_base(), "/api/pinconvers/list", &payload)
            .await
    }

    /// Pins or unpins threads (`/api/pinconvers/updatev2`).
    ///
    /// `action` is [`PIN_ADD`] or [`PIN_REMOVE`]. Ids are prefixed `u`/`g` as Zalo PC does.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_pinned_conversations(
        &self,
        thread_ids: &[String],
        action: i64,
    ) -> Result<Value, ClientError> {
        let conversations: Vec<String> = thread_ids.iter().map(|id| pin_id(id)).collect();
        let payload = json!({
            "actionType": action,
            "conversations": conversations,
        });
        self.post_encrypted(
            self.conversation_base(),
            "/api/pinconvers/updatev2",
            &payload,
        )
        .await
    }

    /// Mutes or unmutes a thread (`/api/social/profile/setmute`).
    ///
    /// `action` is [`SET_MUTE`] or [`UNSET_MUTE`]. `duration` −1 means until cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn set_mute(
        &self,
        thread_id: &str,
        is_group: bool,
        action: i64,
        duration: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "toid": strip_group(thread_id),
            "duration": duration,
            "action": action,
            "startTime": unix_now(),
            "muteType": if is_group { 2 } else { 1 },
            "imei": self.device.imei,
        });
        self.post_encrypted(
            &self.profile_base(),
            "/api/social/profile/setmute",
            &payload,
        )
        .await
    }

    /// Writes conversation labels (`/api/convlabel/update`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_conversation_label(
        &self,
        label_data: &str,
        version: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "labelData": label_data,
            "version": version,
            "imei": self.device.imei,
        });
        self.post_encrypted(&self.label_base(), "/api/convlabel/update", &payload)
            .await
    }

    /// Moves threads between Focused and Other (`/api/archivedchat/update`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_archived_chats(
        &self,
        ids: &[String],
        version: i64,
        action_type: i64,
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "ids": ids,
            "version": version,
            "actionType": action_type,
            "imei": self.device.imei,
        });
        self.post_encrypted(&self.label_base(), "/api/archivedchat/update", &payload)
            .await
    }

    /// Invites members (`/api/group/invite/v2`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn invite_group_members(
        &self,
        grid: &str,
        members: &[String],
    ) -> Result<Value, ClientError> {
        let types = vec![0; members.len()];
        let payload = json!({
            "grid": strip_group(grid),
            "members": members,
            "memberTypes": types,
            "imei": self.device.imei,
            "clientLang": "vi",
        });
        self.post_encrypted(&self.group_base(), "/api/group/invite/v2", &payload)
            .await
    }

    /// Leaves a group (`/api/group/leave`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn leave_group(&self, grid: &str, silent: bool) -> Result<Value, ClientError> {
        let payload = json!({
            "grids": [strip_group(grid)],
            "imei": self.device.imei,
            "silent": if silent { 1 } else { 0 },
            "language": "vi",
        });
        self.post_encrypted(&self.group_base(), "/api/group/leave", &payload)
            .await
    }

    /// Renames a group (`/api/group/updateinfo`, zca-js `changeGroupName`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn rename_group(&self, grid: &str, name: &str) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "gname": name,
            "imei": self.device.imei,
        });
        self.post_encrypted(&self.group_base(), "/api/group/updateinfo", &payload)
            .await
    }

    /// Removes members (`/api/group/kickout`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn kick_group_members(
        &self,
        grid: &str,
        members: &[String],
    ) -> Result<Value, ClientError> {
        let payload = json!({
            "grid": strip_group(grid),
            "members": members,
            "imei": self.device.imei,
        });
        self.post_encrypted(&self.group_base(), "/api/group/kickout", &payload)
            .await
    }

    /// Logs out this PC session (`/api/v2/login/logOut`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn logout(&self) -> Result<Value, ClientError> {
        let payload = json!({
            "time": unix_now(),
            "imei": self.device.imei,
            "computer_name": self.device.computer_name,
            "language": "vi",
        });
        self.post_encrypted(&self.auth_base, "/api/v2/login/logOut", &payload)
            .await
    }

    /// Group profile and a page of members (`/api/group/getgi/v2`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_info_v2(&self, grid: &str, page: i64) -> Result<Value, ClientError> {
        let payload = group_info_payload(grid, page, &self.device.imei);
        self.get_encrypted(&self.group_base(), "/api/group/getgi/v2", &payload)
            .await
    }

    /// Archived-chat list (`/api/archivedchat/list`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_archived_chats(&self, version: i64) -> Result<Value, ClientError> {
        let payload = json!({
            "version": version,
            "imei": self.device.imei,
        });
        self.get_encrypted(&self.label_base(), "/api/archivedchat/list", &payload)
            .await
    }

    /// Sends a user contact / namecard (`/api/message/contact` or `/api/group/contact`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_contact(
        &self,
        thread_id: &str,
        is_group: bool,
        contact_uid: &str,
        client_id: u64,
    ) -> Result<Value, ClientError> {
        if is_group {
            let payload =
                contact_group_payload(thread_id, contact_uid, client_id, &self.device.imei);
            self.post_encrypted(&self.group_base(), "/api/group/contact", &payload)
                .await
        } else {
            let payload =
                contact_direct_payload(thread_id, contact_uid, client_id, &self.device.imei);
            self.post_encrypted(&self.chat_base(), "/api/message/contact", &payload)
                .await
        }
    }

    /// Sends a plain-text message carrying `metaData.urgency` (`0`, `1`, `2`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_text_with_urgency(
        &self,
        thread_id: &str,
        is_group: bool,
        message: &str,
        client_id: u64,
        urgency: u8,
    ) -> Result<Value, ClientError> {
        if urgency == 0 {
            return if is_group {
                self.send_group_message(thread_id, message, client_id).await
            } else {
                self.send_message(thread_id, message, client_id).await
            };
        }
        let payload = text_urgency_payload(
            thread_id,
            is_group,
            message,
            client_id,
            &self.device.imei,
            urgency,
        );
        let (base, path) = if is_group {
            (self.group_base(), "/api/group/sendmsg")
        } else {
            (self.chat_base(), "/api/message/sms")
        };
        self.post_encrypted(&base, path, &payload).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_chunks_are_three_mib() {
        assert_eq!(ASYNC_CHUNK_BYTES, 3 * 1024 * 1024);
        assert_eq!(async_chunk_count(0), 0);
        assert_eq!(async_chunk_count(1), 1);
        assert_eq!(async_chunk_count(ASYNC_CHUNK_BYTES), 1);
        assert_eq!(async_chunk_count(ASYNC_CHUNK_BYTES + 1), 2);
    }

    fn quote() -> QuoteTarget {
        QuoteTarget {
            from_uid: "111".into(),
            msg_id: 99,
            cli_msg_id: 88,
            cli_msg_type: MSG_TEXT,
            ts: 1_700_000_000_000,
            qmsg: "hello".into(),
        }
    }

    #[test]
    fn quote_payloads_match_bundle_keys() {
        let q = quote();
        let d = quote_direct_payload("222", "reply", 1, "imei", &q);
        assert_eq!(d["toid"], "222");
        assert_eq!(d["qmsgOwner"], "111");
        assert_eq!(d["qmsgId"], 99);
        assert_eq!(d["qmsgCliId"], 88);
        assert_eq!(d["qmsgType"], 1);
        assert_eq!(d["qmsg"], "hello");
        assert_eq!(d["imei"], "imei");
        let g = quote_group_payload("g_9", "reply", 1, &q);
        assert_eq!(g["grid"], "9");
        assert_eq!(g["visibility"], 0);
        assert_eq!(g["language"], "vi");
    }

    #[test]
    fn video_forward_payload_matches_bundle() {
        let info = VideoSendInfo {
            title: "clip.mp4".into(),
            video_url: "https://v.example/a.mp4".into(),
            thumb_url: "https://v.example/a.jpg".into(),
            duration_ms: 65_000,
            file_size: 1000,
            width: 720,
            height: 1280,
        };
        let direct = forward_video_payload("222", false, 9, "imei-1", &info);
        assert_eq!(direct["toId"], "222");
        assert_eq!(direct["imei"], "imei-1");
        assert_eq!(direct["msgType"], MSGTYPE_FORWARD_VIDEO);
        assert_eq!(direct["clientId"], 9);
        let msg: Value = serde_json::from_str(direct["msgInfo"].as_str().unwrap()).unwrap();
        assert_eq!(msg["videoUrl"], "https://v.example/a.mp4");
        assert_eq!(msg["thumbUrl"], "https://v.example/a.jpg");
        assert_eq!(msg["duration"], 65_000);
        assert_eq!(msg["width"], 720);
        assert_eq!(msg["height"], 1280);
        assert_eq!(msg["fileSize"], 1000);
        assert_eq!(msg["properties"], "{}");
        let bare = VideoSendInfo {
            thumb_url: String::new(),
            duration_ms: 0,
            width: 0,
            height: 0,
            ..info
        };
        let group = forward_video_payload("g_9", true, 9, "imei-1", &bare);
        assert_eq!(group["grid"], "9");
        assert_eq!(group["visibility"], 0);
        assert!(group.get("imei").is_none());
        let msg: Value = serde_json::from_str(group["msgInfo"].as_str().unwrap()).unwrap();
        assert_eq!(msg["duration"], 1);
        assert!(msg.get("thumbUrl").is_none());
        assert!(msg.get("width").is_none());
    }

    #[test]
    fn undo_and_delete_payloads_match_bundle() {
        let u = undo_direct_payload("222", 10, 11, 12);
        assert_eq!(u["msgId"], 10);
        assert_eq!(u["cliMsgIdUndo"], 12);
        let ug = undo_group_payload("g_9", 10, 11, 12, "imei");
        assert_eq!(ug["grid"], "9");
        assert_eq!(ug["imei"], "imei");
        let item = DeleteItem {
            cli_msg_id: 12,
            global_msg_id: 10,
            owner_id: "me".into(),
            dest_id: Some("222".into()),
        };
        let d = delete_direct_payload("222", 12, &[item], true, "imei");
        assert_eq!(d["onlyMe"], 1);
        assert_eq!(d["msgs"][0]["cliMsgId"], "12");
        assert_eq!(d["msgs"][0]["globalMsgId"], "10");
        assert_eq!(d["msgs"][0]["destId"], "222");
        let group_item = DeleteItem {
            cli_msg_id: 34,
            global_msg_id: 56,
            owner_id: "peer".into(),
            dest_id: Some("g_9".into()),
        };
        let dg = delete_group_payload("g_9", 12, &[group_item], false);
        assert_eq!(dg["onlyMe"], 0);
        assert_eq!(dg["grid"], "9");
        assert_eq!(dg["msgs"][0]["cliMsgId"], "34");
        assert_eq!(dg["msgs"][0]["globalMsgId"], "56");
        assert_eq!(dg["msgs"][0]["destId"], "9");
    }

    #[test]
    fn forward_and_reaction_payloads() {
        let f = forward_direct_payload("222", "hi", 1, "imei");
        assert_eq!(f["toId"], "222");
        assert_eq!(f["msgType"], MSG_TEXT);
        assert!(f["msgInfo"].as_str().unwrap().contains("hi"));
        let item = reaction_item(10, 12, MSG_TEXT, &Reaction::like(), 3);
        assert_eq!(item["clientId"], 3);
        let inner: Value = serde_json::from_str(item["message"].as_str().unwrap()).unwrap();
        assert_eq!(inner["rType"], REACT_LIKE);
        assert_eq!(inner["rIcon"], REACT_LIKE_ICON);
        assert_eq!(inner["rMsg"][0]["gMsgID"], 10);
        assert_eq!(inner["source"], 6);
    }

    #[test]
    fn pin_ids_get_u_or_g_prefix() {
        assert_eq!(pin_id("123"), "u123");
        assert_eq!(pin_id("g_9"), "g9");
        assert_eq!(pin_id("g9"), "g9");
        assert_eq!(pin_id("u123"), "u123");
    }

    #[test]
    fn sharefile_and_voice_payloads_match_bundle() {
        let d = sharefile_payload("222", false, 99);
        assert_eq!(d["toid"], "222");
        assert_eq!(d["clientId"], "99");
        let g = sharefile_payload("g_9", true, 99);
        assert_eq!(g["grid"], "9");
        assert_eq!(g.get("toid"), None);
        let v = voice_payload("g_9", true, 7);
        assert_eq!(v["grid"], "9");
        assert_eq!(v["clientId"], "7");
        assert_eq!(SHAREFILE_MAX_BYTES, 100 * 1024 * 1024);
        assert_eq!(MSG_FILE, 19);
        assert_eq!(MSG_VOICE, 3);
        assert!(!voice_href_is_amr("https://voice.example/a.aac"));
        assert!(voice_href_is_amr("https://voice.example/a.amr?x=1"));
        assert_eq!(
            voice_play_href("https://voice.example/a.aac", Some(9)),
            "https://voice.example/a.aac"
        );
        assert_eq!(
            voice_play_href("https://voice.example/a.amr", Some(9)),
            "https://cache.voice.talk.zdn.vn/mp3?url=https%3A%2F%2Fvoice.example%2Fa.amr&msgid=9"
        );
    }

    #[test]
    fn group_info_and_archive_ids() {
        let p = group_info_payload("g_9", 1, "imei");
        assert_eq!(p["grid"], "9");
        assert_eq!(p["mpage"], 1);
        assert_eq!(p["mcount"], 100);
        assert_eq!(p["avatar_size"], 240);
        assert_eq!(p["imei"], "imei");
        assert_eq!(conv_id("g_9"), "g9");
        assert_eq!(conv_id("123"), "123");
        assert_eq!(ARCHIVE_ADD, 0);
        assert_eq!(ARCHIVE_REMOVE, 1);
    }

    #[test]
    fn contact_and_urgency_payloads_match_bundle() {
        let d = contact_direct_payload("222", "333", 10, "imei");
        assert_eq!(d["toId"], "222");
        assert_eq!(d["contactId"], "333");
        assert_eq!(d["clientId"], 10);
        assert_eq!(d["imei"], "imei");
        let g = contact_group_payload("g_9", "333", 11, "imei");
        assert_eq!(g["grid"], "9");
        assert_eq!(g["visibility"], 0);
        assert_eq!(g["contactId"], "333");
        let norm = text_urgency_payload("222", false, "hi", 12, "imei", 0);
        assert_eq!(norm.get("metaData"), None);
        let imp = text_urgency_payload("g_9", true, "gấp", 13, "imei", 2);
        assert_eq!(imp["grid"], "9");
        assert_eq!(imp["metaData"]["urgency"], 2);
    }

    #[test]
    fn avatar_and_online_payloads_match_bundle() {
        let bare = profile_avatar_payload(9, 0, 0, 0);
        assert_eq!(bare["avatarSize"], 160);
        assert_eq!(bare["clientId"], 9);
        assert_eq!(bare["language"], "vi");
        assert!(bare.get("metaData").is_none());
        assert!(bare.get("imei").is_none());
        let full = profile_avatar_payload(9, 320, 240, 8000);
        let meta: Value = serde_json::from_str(full["metaData"].as_str().unwrap()).unwrap();
        assert_eq!(meta["origin"]["width"], 320);
        assert_eq!(meta["processed"]["size"], 8000);
        let group = group_avatar_payload("g_42", 7, 0, 0, "imei-1");
        assert_eq!(group["grid"], "42");
        assert_eq!(group["avatarSize"], 160);
        assert_eq!(group["originWidth"], 240);
        assert_eq!(group["originHeight"], 240);
        assert_eq!(group["imei"], "imei-1");
        assert!(avatar_len_error(100).is_some());
        assert!(avatar_len_error(8 * 1024).is_none());
        assert!(avatar_len_error((30 * 1024 * 1024 + 1) as usize).is_some());

        let ids = friend_online_ids(&json!({
            "onlines": [
                {"userId": "11"},
                {"userId": "0"},
                {"userId": 11},
                {"userId": "22"}
            ]
        }));
        assert_eq!(ids, vec!["11".to_owned(), "22".to_owned()]);
        let seen =
            last_online_from(&json!({"lastOnline": 1700, "settings": {"show_online_status": 1}}));
        assert_eq!(
            seen,
            LastOnline {
                at_ms: 1700,
                hidden: false
            }
        );
        let hidden = last_online_from(
            &json!({"data": {"lastOnline": "5", "settings": {"show_online_status": false}}}),
        );
        assert!(hidden.hidden);
        assert_eq!(hidden.at_ms, 0);
        assert_eq!(
            avatar_url_from(&json!({"data": {"avatar": "https://cdn.example/a.jpg"}})),
            "https://cdn.example/a.jpg"
        );
        assert_eq!(CONV_TYPE_USER, 1);
        assert_eq!(CONV_TYPE_GROUP, 0);
        assert_eq!(CONV_TYPE_OA, 2);
        assert_eq!(AVATAR_ERR_NO_PERMISSION, 166);
        assert_eq!(AVATAR_ERR_OVERSIZE, 200);
    }

    #[test]
    fn media_store_payloads_match_bundle() {
        let group = media_list_payload(
            false,
            "g_9",
            MEDIA_TYPE_IMAGE,
            Some(50),
            Some("8"),
            None,
            "imei",
        );
        assert_eq!(group["media_type"], "media");
        assert_eq!(group["group_id"], "9");
        assert_eq!(group["imei"], "imei");
        assert_eq!(group["limit"], 50);
        assert_eq!(group["last_id"], "8");
        assert!(group.get("lastFetchId").is_none());
        let mine = media_list_payload(true, "999", MEDIA_TYPE_FILE, None, None, Some(3), "imei");
        assert_eq!(mine["media_type"], "file");
        assert_eq!(mine["lastFetchId"], 3);
        assert!(mine.get("group_id").is_none());
        assert!(mine.get("imei").is_none());
        assert!(mine.get("limit").is_none());
        let preview = media_preview_payload(false, "g_9", 0, "imei");
        assert_eq!(preview["limit"], 50);
        assert_eq!(preview["group_id"], "9");
        let preview_me = media_preview_payload(true, "999", 20, "imei");
        assert_eq!(preview_me["limit"], 20);
        assert!(preview_me.get("imei").is_none());
        let remove = media_remove_payload(false, "g_9", MEDIA_TYPE_LINK, "m1");
        assert_eq!(remove["media_type"], "link");
        assert_eq!(remove["media_id"], "m1");
        assert_eq!(remove["group_id"], "9");
        assert!(remove.get("imei").is_none());
        assert!(media_remove_payload(true, "999", "media", "m2")
            .get("group_id")
            .is_none());

        let page = media_page_from(&json!({
            "error_code": 0,
            "data": {
                "items": [
                    {
                        "id": "m1", "type": "media", "sub_type": 2,
                        "global_msg_id": 99, "client_msg_id": "7",
                        "src_url": "https://cdn.example/a.mp4",
                        "thumb_url": "https://cdn.example/a.jpg",
                        "file_name": "a.mp4", "file_size": 1200,
                        "width": 640, "height": 480
                    },
                    {"id": 5, "type": "file", "file_name": "b.pdf", "src_url": "https://cdn.example/b.pdf", "file_size": "2048"}
                ],
                "has_more": 1,
                "last_id": 55
            }
        }));
        assert!(page.has_more);
        assert_eq!(page.last_id, "55");
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].kind, "image");
        assert_eq!(page.items[0].sub_type, 2);
        assert_eq!(page.items[0].msg_id, "99");
        assert_eq!(page.items[0].file_size, 1200);
        assert_eq!(page.items[1].id, "5");
        assert_eq!(page.items[1].file_size, 2048);
        let preview = media_preview_from(&json!({
            "data": [
                {"id": "p", "type": "media", "sub_type": 1, "src_url": "https://cdn.example/p.jpg"},
                {"id": "f", "type": "file", "title": "c.zip", "src_url": "https://cdn.example/c.zip"},
                {"id": "l", "type": "link", "title": "Tin", "src_url": "https://example.com"},
                {"id": "x", "type": "noise"}
            ]
        }));
        assert_eq!(preview.images.len(), 1);
        assert_eq!(preview.files[0].title, "c.zip");
        assert_eq!(preview.links[0].url, "https://example.com");
        assert_eq!(MEDIA_ERR_GONE, 212);
    }
}
