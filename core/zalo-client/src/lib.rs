//! Headless Rust client for Zalo Web/PC.
//!
//! The first slice covers device identity, the unauthenticated server lookup,
//! QR login, local session persistence, and the realtime `zpw_ws` listener.
//! Calls are implemented in the zcall crates.

#![forbid(unsafe_code)]

pub mod api;
pub mod device;
pub mod features;
pub mod group3;
pub mod listener;
pub mod ops;
pub mod qr;
pub mod session;
pub mod status;
pub mod voicecall;

pub use api::{
    archived_ids_from, confirm_zpw_sek, ClientError, ConversationLabel,
    FileUpload, FriendAnswer, GifRef, GroupMember, LoginInfo, PhotoUpload, QrScanOutcome,
    QrTicket, StickerPack, StickerRef, ZaloApi,
};
pub use device::{Device, DeviceError};
pub use features::{
    GroupSettingsPatch, ThreadMsgRef, BOARD_ALL, EVENT_ACCEPT, EVENT_NONE, EVENT_REJECT,
    REPORT_TYPE_GROUP, REPORT_TYPE_USER, TOPIC_REMINDER, TOPIC_TEXT,
};
pub use group3::{
    block_sharing_ids, parse_blocked_group_members, parse_group_link_preview, parse_pin_topics,
    parse_todo_list, BlockedGroupMember, GroupLinkPreview, PinTopicEntry, TodoCreateInput,
    TodoItem, TODO_STATUS_DONE, TODO_STATUS_OPEN, TODO_TYPE_DEFAULT,
};
pub use listener::{
    run_listener, run_listener_with, Frame, ListenerConfig, ListenerError, ListenerEvent,
    CMD_GET_STATUS_MSG, CMD_GET_STATUS_MSG_GROUP,
};
pub use ops::{
    avatar_url_from, conv_id, get_status_msg_ws_body, parse_status_msg_response,
    voice_href_is_amr, voice_play_href, DeleteItem, LastOnline, LinkSendInfo, MediaItem, MediaPage,
    MediaPreview, MemberMsgStatus, MentionItem, MultiUndoItem, PhotoUrlSendInfo, QuoteTarget,
    Reaction, RenewLinkRequest, RenewLinkResult, ScanUrlResult, ARCHIVE_ADD, ARCHIVE_REMOVE,
    AVATAR_ERR_NO_PERMISSION, AVATAR_ERR_OVERSIZE, AVATAR_MAX_BYTES, AVATAR_MIN_BYTES,
    AVATAR_MIN_EDGE, AVATAR_SIZE, CONV_TYPE_GROUP, CONV_TYPE_OA, CONV_TYPE_USER, MEDIA_ERR_GONE,
    MEDIA_PREVIEW_LIMIT, MEDIA_STORE_FALLBACK, MEDIA_STORE_SEND2ME_FALLBACK, MEDIA_TYPE_FILE,
    MEDIA_TYPE_IMAGE, MEDIA_TYPE_LINK, MSGTYPE_FORWARD_FILE, MSGTYPE_FORWARD_GIF,
    MSGTYPE_FORWARD_PHOTO, MSGTYPE_FORWARD_VIDEO, MSGTYPE_FORWARD_VOICE, MSG_FILE, MSG_TEXT,
    MSG_VOICE, PIN_ADD, PIN_REMOVE, SET_MUTE, SHAREFILE_MAX_BYTES, UNSET_MUTE,
    VOICE_DIRECT_MAX_BYTES, VOICE_MP3_HOST,
};
pub use qr::{render_terminal, QrError};
pub use session::{SessionError, StoredSession};
pub use status::{seen_record, MemberReceipt, ReadMark, SeenMark, StatusUpdate};
pub use voicecall::{
    control_frame, incoming_call_from_control, recv_signal_from_http, send_signal_parts,
    voicecall_from_frame, voicecall_from_signal, voicecall_http_payload, voip_control_is_fresh,
    voip_controls_from_event, IncomingCall, SIG_ANSWER, SIG_ANSWER_ACK, SIG_CANCEL, SIG_CONF,
    SIG_END, SIG_LOG_END, SIG_REQUEST_CALL, SIG_RING, SIG_SEND_REQUEST, VOICE_CALL_FALLBACK,
};

#[cfg(test)]
mod test_support;
