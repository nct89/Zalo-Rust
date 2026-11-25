//! User-click bridge to `zalo-client`. Never runs on GPUI startup.
//!
//! Preview thread ids (`preview:*`, `1`–`4`) are rejected. Tokio lives on a
//! dedicated current-thread worker so GPUI's executor is not mixed in.

use std::path::PathBuf;

use zalo_client::{
    conv_id, DeleteItem, Device, GifRef, LinkSendInfo, MentionItem, MultiUndoItem,
    PhotoUrlSendInfo, QuoteTarget, Reaction, StickerRef, StoredSession, ZaloApi, ARCHIVE_ADD,
    ARCHIVE_REMOVE, MSG_FILE, MSG_TEXT, MSG_VOICE, PIN_ADD, PIN_REMOVE, SET_MUTE, UNSET_MUTE,
    VOICE_DIRECT_MAX_BYTES,
};

use crate::data::live_allowed;

/// One user-initiated protocol call.
#[derive(Clone, Debug)]
pub enum LiveAction {
    /// Plain text, or quote when `quote` is set (`urgency`: 0 normal, 1 important, 2 urgent).
    Send {
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Body.
        text: String,
        /// Quote target when the composer reply bar is set and ids exist.
        quote: Option<QuoteTarget>,
        /// Structured group `@mention` spans (UTF-16 offsets).
        mentions: Vec<MentionItem>,
        /// Client language (`"vi"` / `"en"`).
        language: String,
        /// Priority tag (`0` normal, `1` important, `2` urgent).
        urgency: u8,
        /// `clientId` / `cliMsgId`.
        client_id: u64,
    },
    /// Send a friend contact / namecard (`/api/message/contact` or `/api/group/contact`).
    SendContact {
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Contact user id to share.
        contact_uid: String,
        /// `clientId` / `cliMsgId`.
        client_id: u64,
    },
    /// Pin (`true`) or unpin.
    Pin {
        /// Thread to pin.
        thread_id: String,
        /// Pin when true.
        pin: bool,
    },
    /// Mute (`true`) or unmute.
    Mute {
        /// Thread to mute.
        thread_id: String,
        /// Group vs 1-1 (`muteType`).
        is_group: bool,
        /// Mute when true.
        mute: bool,
        /// Seconds; −1 until the user unmutes (`STR_MUTE_NO_LIMIT`).
        duration: i64,
    },
    /// Focused / Other.
    Archive {
        /// Thread to move.
        thread_id: String,
        /// Move to Other when true.
        archive: bool,
    },
    /// Recall a message we sent.
    Undo {
        /// Parent thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Server id.
        msg_id: i64,
        /// New client id of the undo request.
        client_id: u64,
        /// `cliMsgId` of the recalled bubble.
        cli_msg_id_undo: i64,
    },
    /// Recall multiple messages in one call (`/api/message/multi_undo` or `/api/group/multi_undomsg`).
    UndoMulti {
        /// Parent thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Recalled message descriptors.
        items: Vec<MultiUndoItem>,
    },
    /// Delete for me.
    Delete {
        /// Parent thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// New client id of the delete request.
        cli_msg_id: i64,
        /// Row in `msgs`.
        item: DeleteItem,
    },
    /// Message reaction (`/-strong`, `/-heart`, `:>` … or clear with `r_type == -1`).
    React {
        /// Parent thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Server id.
        msg_id: i64,
        /// Client id of the target bubble.
        cli_msg_id: i64,
        /// Zalo message type (`MSG_TEXT`, `MSG_VOICE`, `MSG_FILE`).
        msg_type: i64,
        /// Wire reaction (`r_type`, `r_icon`).
        reaction: Reaction,
        /// New client id of the reaction request.
        client_id: u64,
    },
    /// Forward text to another thread.
    Forward {
        /// Destination thread.
        dest_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Body.
        text: String,
        /// `clientId`.
        client_id: u64,
    },
    /// Forward text to multiple conversations in one batch (`/api/message/mforward` & `/api/group/mforward`).
    ForwardMulti {
        /// `(thread_id, is_group)` destinations.
        targets: Vec<(String, bool)>,
        /// Body.
        text: String,
        /// Starting `clientId`.
        base_client_id: u64,
    },
    /// Block messages and calls (`/api/friend/block`).
    Block {
        /// Friend uid.
        uid: String,
    },
    /// Upload + send one photo (`upload_photo` → `send_photo`).
    SendPhoto {
        /// `clientId` shared with the local bubble (dedupes the echo).
        client_id: u64,
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Local image file (PNG/JPEG/…).
        path: PathBuf,
        /// Caption (may be empty).
        caption: String,
        /// Electron `groupLayoutId`. `0` when this photo is not part of an album.
        group_layout_id: i64,
        /// `totalItemInGroup`.
        group_total: u32,
        /// `idInGroup`.
        group_index: u32,
    },
    /// Send a cached/existing photo by URL without re-uploading (`/api/*/photo_url`).
    SendPhotoByUrl {
        /// `clientId` shared with the local bubble.
        client_id: u64,
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Photo URL metadata (`oriUrl`, `thumbUrl`, `hdUrl`, dimensions, title).
        info: PhotoUrlSendInfo,
    },
    /// Upload + send one file (`sharefile_full`).
    SendFile {
        /// `clientId` shared with the local bubble.
        client_id: u64,
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Local file.
        path: PathBuf,
    },
    /// Send a sticker from the store / pack / suggestion strip.
    SendSticker {
        /// `clientId` shared with the local bubble.
        client_id: u64,
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Sticker metadata.
        sticker: StickerRef,
    },
    /// Send a GIF from search / trending / categories.
    SendGif {
        /// `clientId` shared with the local bubble.
        client_id: u64,
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// GIF metadata.
        gif: GifRef,
    },
    /// Upload and send a recorded MP3 voice note (`voicemp3` / `group/sendmp3`).
    SendVoice {
        /// `clientId` shared with the local bubble.
        client_id: u64,
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Encoded MP3 bytes.
        bytes: Vec<u8>,
        /// File name passed to multipart upload (e.g. `"voice.mp3"`).
        file_name: String,
    },
    /// Upload a video (`video/file/upload`), wait for `video_done`, then
    /// forward with `msgType` 5 on the file host.
    SendVideo {
        /// `clientId` shared with the local bubble.
        client_id: u64,
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Local video file.
        path: PathBuf,
        /// Length in milliseconds (`duration || 1`).
        duration_ms: u64,
        /// Pixel width. `0` omits `width` on the forward body.
        width: u32,
        /// Pixel height. `0` omits `height` on the forward body.
        height: u32,
        /// JPEG poster from `ffmpeg`, when one was written.
        thumb: Option<PathBuf>,
    },
    /// Send a read receipt (`/api/message/seenv2` or `/api/group/seenv2`).
    Seen {
        /// Destination thread.
        thread_id: String,
        /// Raw `seen_record` payload `{cmi, gmi, si, di, mt, st, at, cmd, ts}`.
        record: serde_json::Value,
    },
    /// Send a delivery receipt (`/api/message/deliveredv2` or `/api/group/deliveredv2`).
    Delivered {
        /// Destination thread.
        thread_id: String,
        /// Same per-message record as [`LiveAction::Seen`].
        record: serde_json::Value,
    },
    /// Send a typing indicator (`/api/message/typing` or `/api/group/typing`).
    Typing {
        /// Destination thread.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
    },
    /// Persist an unread badge on server (`/api/conv/addUnreadMark`).
    MarkUnread {
        /// Thread and last message reference.
        item: zalo_client::ThreadMsgRef,
    },
    /// Clear server unread mark (`/api/conv/removeUnreadMark`).
    RemoveUnread {
        /// Conversation `thread_id`.
        thread_id: String,
        /// Group vs 1-1.
        is_group: bool,
        /// Timestamp (ms).
        ts: i64,
    },
    /// Delete a conversation on server (`/api/message/deleteconver` or `/api/group/deleteconver`).
    DeleteConv {
        /// Thread and last message reference.
        item: zalo_client::ThreadMsgRef,
        /// Request client id.
        client_id: u64,
    },
    /// Remove a friend (`/api/friend/remove`).
    RemoveFriend {
        /// Peer user id.
        uid: String,
    },
    /// Set or clear a friend's alias (`/api/alias/update` or `/api/alias/remove`).
    UpdateAlias {
        /// Peer user id.
        uid: String,
        /// New alias; empty removes it.
        alias: String,
    },
    /// Report a user or group (`/api/social/profile/reportabuse`).
    ReportAbuse {
        /// User uid or `g_<id>`.
        target_id: String,
        /// True for group threads.
        is_group: bool,
        /// Zalo abuse reason code.
        reason: i64,
        /// Optional description.
        content: String,
    },
    /// Pin a message/note onto the group board (`/api/board/pinv2`).
    PinBoard {
        /// Group thread id.
        thread_id: String,
        /// Topic/message id to pin.
        topic_id: String,
        /// `zalo_client::GROUP_BOARD_*` type.
        item_type: i64,
    },
    /// Unpin an item from the group board (`/api/board/unpinv2`).
    UnpinBoard {
        /// Group thread id.
        thread_id: String,
        /// Topic/message id to unpin.
        topic_id: String,
        /// Board version if known (`0` default).
        board_version: i64,
    },
    /// Submit votes for a group poll (`/api/poll/vote`).
    VotePoll {
        /// Server poll id.
        poll_id: i64,
        /// Selected option ids.
        option_ids: Vec<i64>,
    },
}

/// Fire-and-forget. No-op when the thread id is a preview fixture.
pub fn spawn_live(action: LiveAction, session_dir: PathBuf) {
    if let LiveAction::Send { thread_id, .. }
    | LiveAction::SendContact { thread_id, .. }
    | LiveAction::Pin { thread_id, .. }
    | LiveAction::Mute { thread_id, .. }
    | LiveAction::Archive { thread_id, .. }
    | LiveAction::Undo { thread_id, .. }
    | LiveAction::UndoMulti { thread_id, .. }
    | LiveAction::Delete { thread_id, .. }
    | LiveAction::React { thread_id, .. }
    | LiveAction::Forward {
        dest_id: thread_id, ..
    }
    | LiveAction::SendPhoto { thread_id, .. }
    | LiveAction::SendPhotoByUrl { thread_id, .. }
    | LiveAction::SendFile { thread_id, .. }
    | LiveAction::SendSticker { thread_id, .. }
    | LiveAction::SendGif { thread_id, .. }
    | LiveAction::SendVoice { thread_id, .. }
    | LiveAction::SendVideo { thread_id, .. }
    | LiveAction::Seen { thread_id, .. }
    | LiveAction::Delivered { thread_id, .. }
    | LiveAction::Typing { thread_id, .. }
    | LiveAction::RemoveUnread { thread_id, .. }
    | LiveAction::PinBoard { thread_id, .. }
    | LiveAction::UnpinBoard { thread_id, .. }
    | LiveAction::MarkUnread {
        item: zalo_client::ThreadMsgRef { thread_id, .. },
    }
    | LiveAction::DeleteConv {
        item: zalo_client::ThreadMsgRef { thread_id, .. },
        ..
    }
    | LiveAction::ReportAbuse {
        target_id: thread_id,
        ..
    } = &action
    {
        if !live_allowed(thread_id) {
            return;
        }
    }
    if let LiveAction::Block { uid }
    | LiveAction::RemoveFriend { uid }
    | LiveAction::UpdateAlias { uid, .. } = &action
    {
        if !live_allowed(uid) {
            return;
        }
    }
    let label = match &action {
        LiveAction::SendPhoto { .. } | LiveAction::SendPhotoByUrl { .. } => Some("Gửi ảnh"),
        LiveAction::SendFile { .. } => Some("Gửi file"),
        LiveAction::SendSticker { .. } => Some("Gửi sticker"),
        LiveAction::SendGif { .. } => Some("Gửi GIF"),
        LiveAction::SendVoice { .. } => Some("Gửi thoại"),
        LiveAction::SendVideo { .. } => Some("Gửi video"),
        LiveAction::SendContact { .. } => Some("Gửi danh thiếp"),
        LiveAction::Send { .. } => Some("Gửi tin"),
        LiveAction::Forward { .. } | LiveAction::ForwardMulti { .. } => Some("Chia sẻ"),
        LiveAction::UndoMulti { .. } => Some("Thu hồi"),
        _ => None,
    };
    let silent = matches!(
        &action,
        LiveAction::Seen { .. } | LiveAction::Delivered { .. } | LiveAction::Typing { .. }
    );
    let outbox_candidate = match &action {
        LiveAction::Send {
            thread_id,
            is_group,
            text,
            quote,
            urgency,
            client_id,
            ..
        } => Some((
            thread_id.clone(),
            *is_group,
            text.clone(),
            quote.clone(),
            *urgency,
            *client_id,
        )),
        _ => None,
    };
    let _ = std::thread::Builder::new()
        .name("zalo-gpui-bridge".into())
        .spawn(move || {
            let started = std::time::Instant::now();
            match run_live(action, session_dir.clone()) {
                Ok(()) => {
                    if let Some(l) = label {
                        eprintln!("zalo-gpui bridge: {l} ok in {} ms", started.elapsed().as_millis());
                        if matches!(l, "Gửi file" | "Gửi thoại" | "Gửi video") {
                            report(format!("{l}: đã gửi"));
                        }
                    }
                }
                Err(err) => {
                    eprintln!("zalo-gpui bridge: {err}");
                    if crate::login_flow::is_session_error(&err) {
                        report(
                            "Phiên đăng nhập đã hết hạn — bấm «Đăng nhập lại» trên thanh thông báo để quét QR"
                                .into(),
                        );
                    } else if let Some((thread_id, is_group, text, quote, urgency, client_id)) =
                        outbox_candidate
                    {
                        if is_offline_send_error(&err)
                            && enqueue_offline_send(
                                &session_dir,
                                &thread_id,
                                is_group,
                                &text,
                                quote.as_ref(),
                                urgency,
                                client_id,
                            )
                        {
                            report(
                                "Mất mạng — tin nhắn đã lưu hàng đợi và sẽ tự gửi khi có kết nối"
                                    .into(),
                            );
                        } else if !silent {
                            report(format!(
                                "{} thất bại: {}",
                                label.unwrap_or("Thao tác"),
                                short(&err)
                            ));
                        }
                    } else if !silent {
                        report(format!("{} thất bại: {}", label.unwrap_or("Thao tác"), short(&err)));
                    }
                }
            }
        });
}

fn is_offline_send_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    if lower.contains("zalo error") || crate::login_flow::is_session_error(err) {
        return false;
    }
    zalo_client::listener::is_transient_disconnect(err)
        || lower.contains("dns")
        || lower.contains("network")
        || lower.contains("connect")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("error sending request")
}

fn enqueue_offline_send(
    session_dir: &std::path::Path,
    thread_id: &str,
    is_group: bool,
    text: &str,
    quote: Option<&QuoteTarget>,
    urgency: u8,
    client_id: u64,
) -> bool {
    let Ok(store) = zalo_storage::LocalStore::open(&session_dir.join("store.db")) else {
        return false;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let rec = zalo_storage::OutboxRecord {
        id: 0,
        thread_id: thread_id.to_owned(),
        is_group,
        body: text.to_owned(),
        urgency,
        client_id,
        quote_json: quote.and_then(|q| serde_json::to_string(q).ok()),
        created_at: now_ms,
        attempts: 0,
    };
    store.enqueue_outbox(&rec).is_ok()
}

static OUTBOX_FLUSHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Flushes queued offline text messages in FIFO order on reconnect (`RtEvent::Online`).
pub fn flush_outbox(session_dir: PathBuf) {
    if OUTBOX_FLUSHING
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("zalo-gpui-outbox".into())
        .spawn(move || {
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    OUTBOX_FLUSHING.store(false, std::sync::atomic::Ordering::Release);
                }
            }
            let _guard = Guard;
            let store_path = session_dir.join("store.db");
            let Ok(store) = zalo_storage::LocalStore::open(&store_path) else {
                return;
            };
            let Ok(pending) = store.list_outbox() else {
                return;
            };
            if pending.is_empty() {
                return;
            }
            let mut sent = 0usize;
            for item in pending {
                if item.attempts >= 5 {
                    let _ = store.remove_outbox(item.id);
                    continue;
                }
                let quote = item
                    .quote_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<QuoteTarget>(raw).ok());
                let action = LiveAction::Send {
                    thread_id: item.thread_id.clone(),
                    is_group: item.is_group,
                    text: item.body.clone(),
                    quote,
                    mentions: Vec::new(),
                    language: "vi".into(),
                    urgency: item.urgency,
                    client_id: item.client_id,
                };
                match run_live(action, session_dir.clone()) {
                    Ok(()) => {
                        let _ = store.remove_outbox(item.id);
                        sent += 1;
                    }
                    Err(err) => {
                        let _ = store.bump_outbox_attempt(item.id);
                        eprintln!("zalo-gpui outbox: retry failed ({}): {err}", item.id);
                        if is_offline_send_error(&err) || crate::login_flow::is_session_error(&err) {
                            break;
                        }
                    }
                }
            }
            if sent > 0 {
                report(format!("Đã tự động gửi {sent} tin nhắn chờ khi kết nối lại"));
            }
        });
}

#[derive(Clone)]
struct CachedLogin {
    dir: PathBuf,
    sek: String,
    info: zalo_client::LoginInfo,
}

static LOGIN_CACHE: std::sync::Mutex<Option<CachedLogin>> = std::sync::Mutex::new(None);

/// Cache `zpw_service_map_v3` hosts for this active session directory and cookie.
pub(crate) fn remember_login_info(
    dir: &std::path::Path,
    sek: &str,
    info: &zalo_client::LoginInfo,
) {
    if let Ok(mut slot) = LOGIN_CACHE.lock() {
        *slot = Some(CachedLogin {
            dir: dir.to_path_buf(),
            sek: sek.to_owned(),
            info: info.clone(),
        });
    }
}

/// Clears cached `zpw_service_map_v3` hosts (e.g. on logout).
pub(crate) fn clear_login_cache() {
    if let Ok(mut slot) = LOGIN_CACHE.lock() {
        *slot = None;
    }
}

static PENDING_LOGOUT: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(None);

/// Waits for an in-flight `/api/v2/login/logOut` request (if any) to complete
/// before a newly opened QR login requests a fresh ticket for the same device
/// IMEI. Returns `true` when a pending logout was actually waited on.
pub(crate) fn wait_pending_logout() -> bool {
    let handle = PENDING_LOGOUT.lock().ok().and_then(|mut slot| slot.take());
    if let Some(handle) = handle {
        let _ = handle.join();
        true
    } else {
        false
    }
}

/// Fires `/api/v2/login/logOut` on a background thread using an in-memory
/// `(Device, StoredSession)` snapshot captured *before* `session.json` is wiped
/// from disk, avoiding the race where `wipe_session_file` wins before the worker
/// reads `session.json`.
pub(crate) fn spawn_logout_snapshot(
    device: Device,
    session: StoredSession,
    _dir: PathBuf,
) {
    clear_login_cache();
    if cfg!(test) {
        return;
    }
    let handle = std::thread::Builder::new()
        .name("zalo-gpui-logout".into())
        .spawn(move || {
            let Ok(api) = ZaloApi::from_session(device, &session) else {
                return;
            };
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            // `tokio::time::timeout` must be constructed *inside* the Tokio
            // runtime context; evaluating it as an argument to `rt.block_on`
            // panics with "there is no reactor running".
            rt.block_on(async {
                match tokio::time::timeout(std::time::Duration::from_secs(4), api.logout()).await {
                    Ok(Ok(_)) => eprintln!("zalo-gpui logout: server logOut ok"),
                    Ok(Err(err)) => eprintln!("zalo-gpui logout: server logOut err: {err}"),
                    Err(_) => eprintln!("zalo-gpui logout: server logOut timed out"),
                }
            });
        })
        .ok();
    if let Ok(mut slot) = PENDING_LOGOUT.lock() {
        *slot = handle;
    }
}

fn cached_login_info(dir: &std::path::Path, sek: &str) -> Option<zalo_client::LoginInfo> {
    let guard = LOGIN_CACHE.lock().ok()?;
    let cached = guard.as_ref()?;
    (cached.dir == dir && cached.sek == sek).then(|| cached.info.clone())
}

pub(crate) async fn ensure_service_hosts(
    api: &mut ZaloApi,
    dir: &std::path::Path,
    sek: &str,
    force_refresh: bool,
) -> Result<(), String> {
    if !force_refresh {
        if let Some(info) = cached_login_info(dir, sek) {
            api.apply_login_hosts(&info);
            return Ok(());
        }
    }
    let info = api.get_login_info().await.map_err(|e| e.to_string())?;
    remember_login_info(dir, sek, &info);
    Ok(())
}

fn run_live(action: LiveAction, dir: PathBuf) -> Result<(), String> {
    let session = StoredSession::load(&dir).map_err(|e| e.to_string())?;
    let Some(sek) = session.zpw_sek() else {
        return Err("session has no zpw_sek".into());
    };
    let device = Device::load_or_create(&dir).map_err(|e| e.to_string())?;
    let mut api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let needs_fresh_upload_hosts = matches!(
        action,
        LiveAction::SendPhoto { .. }
            | LiveAction::SendFile { .. }
            | LiveAction::SendSticker { .. }
            | LiveAction::SendGif { .. }
            | LiveAction::SendVoice { .. }
            | LiveAction::SendVideo { .. }
    );
    rt.block_on(ensure_service_hosts(
        &mut api,
        &dir,
        &sek,
        needs_fresh_upload_hosts,
    ))?;
    rt.block_on(dispatch(&api, action))
}

/// JPEG for upload: longest side ≤ 2560 px, quality lowered until ≤ 2.5 MB
/// (`zalo-gui` `encode_upload`). Returns bytes, width, height.
fn encode_upload(path: &std::path::Path) -> Result<(Vec<u8>, u32, u32), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let rgba = crate::media::decode_rgba(&bytes, 2560).ok_or("ảnh không đọc được")?;
    let (w, h) = rgba.dimensions();
    let mut rgb = image::RgbImage::new(w, h);
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = u16::from(p[3]);
        let mix = |c: u8| ((u16::from(c) * a + 255 * (255 - a)) / 255) as u8;
        rgb.put_pixel(x, y, image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
    }
    for quality in [90_u8, 80, 70, 60] {
        let mut jpeg = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality);
        rgb.write_with_encoder(enc).map_err(|e| e.to_string())?;
        if jpeg.len() <= 2_500_000 || quality == 60 {
            return Ok((jpeg, w, h));
        }
    }
    Err("không nén được ảnh".into())
}

async fn dispatch(api: &ZaloApi, action: LiveAction) -> Result<(), String> {
    match action {
        LiveAction::Send {
            thread_id,
            is_group,
            text,
            quote,
            mentions,
            language,
            urgency,
            client_id,
        } => {
            if let Some(quote) = quote {
                api.send_quote(&thread_id, is_group, &text, client_id, &quote)
                    .await
            } else if urgency == 0 && crate::media::first_link(&text).is_some() {
                let href = crate::media::first_link(&text).unwrap_or_default();
                let parsed = api.parse_link(&href).await.ok();
                // Match Zalo PC: only upgrade to `sendLinkMessage` when
                // `/api/message/parselink` actually returned rich preview
                // metadata (title/desc/thumb). Otherwise send as a normal
                // mention or text message so unparseable URLs never turn into
                // empty `recommened.link` cards.
                if has_rich_link_metadata(parsed.as_ref(), &href) {
                    let info = build_link_send_info(&text, &href, parsed.as_ref());
                    match api
                        .send_link_message(
                            &thread_id, is_group, &info, &mentions, client_id, &language,
                        )
                        .await
                    {
                        Ok(v) => Ok(v),
                        Err(err) => {
                            eprintln!("zalo-gpui send_link_message fallback to text: {err}");
                            if is_group && !mentions.is_empty() {
                                api.send_group_mention(
                                    &thread_id, &text, &mentions, client_id, &language,
                                )
                                .await
                            } else {
                                api.send_text_with_urgency(
                                    &thread_id, is_group, &text, client_id, urgency,
                                )
                                .await
                            }
                        }
                    }
                } else if is_group && !mentions.is_empty() {
                    match api
                        .send_group_mention(&thread_id, &text, &mentions, client_id, &language)
                        .await
                    {
                        Ok(v) => Ok(v),
                        Err(err) => {
                            eprintln!("zalo-gpui send_group_mention fallback to text: {err}");
                            api.send_text_with_urgency(
                                &thread_id, is_group, &text, client_id, urgency,
                            )
                            .await
                        }
                    }
                } else {
                    api.send_text_with_urgency(&thread_id, is_group, &text, client_id, urgency)
                        .await
                }
            } else if is_group && !mentions.is_empty() && urgency == 0 {
                match api
                    .send_group_mention(&thread_id, &text, &mentions, client_id, &language)
                    .await
                {
                    Ok(v) => Ok(v),
                    Err(err) => {
                        eprintln!("zalo-gpui send_group_mention fallback to text: {err}");
                        api.send_text_with_urgency(&thread_id, is_group, &text, client_id, urgency)
                            .await
                    }
                }
            } else {
                api.send_text_with_urgency(&thread_id, is_group, &text, client_id, urgency)
                    .await
            }
        }
        LiveAction::SendContact {
            thread_id,
            is_group,
            contact_uid,
            client_id,
        } => {
            api.send_contact(&thread_id, is_group, &contact_uid, client_id)
                .await
        }
        LiveAction::Pin { thread_id, pin } => {
            let action = if pin { PIN_ADD } else { PIN_REMOVE };
            api.update_pinned_conversations(&[thread_id], action).await
        }
        LiveAction::Mute {
            thread_id,
            is_group,
            mute,
            duration,
        } => {
            let action = if mute { SET_MUTE } else { UNSET_MUTE };
            api.set_mute(&thread_id, is_group, action, duration).await
        }
        LiveAction::Archive { thread_id, archive } => {
            let action = if archive { ARCHIVE_ADD } else { ARCHIVE_REMOVE };
            let version = api
                .get_archived_chats(0)
                .await
                .ok()
                .map(|v| zalo_client::archived_ids_from(&v).1)
                .unwrap_or(0);
            api.update_archived_chats(&[conv_id(&thread_id)], version, action)
                .await
        }
        LiveAction::Undo {
            thread_id,
            is_group,
            msg_id,
            client_id,
            cli_msg_id_undo,
        } => {
            api.undo_message(&thread_id, is_group, msg_id, client_id, cli_msg_id_undo)
                .await
        }
        LiveAction::UndoMulti {
            thread_id,
            is_group,
            items,
        } => api.undo_multi_messages(&thread_id, is_group, &items).await,
        LiveAction::Delete {
            thread_id,
            is_group,
            cli_msg_id,
            item,
        } => {
            api.delete_messages(&thread_id, is_group, cli_msg_id, &[item], true)
                .await
        }
        LiveAction::React {
            thread_id,
            is_group,
            msg_id,
            cli_msg_id,
            msg_type,
            reaction,
            client_id,
        } => {
            api.send_reaction(
                &thread_id, is_group, msg_id, cli_msg_id, msg_type, &reaction, client_id,
            )
            .await
        }
        LiveAction::Forward {
            dest_id,
            is_group,
            text,
            client_id,
        } => api.forward_text(&dest_id, is_group, &text, client_id).await,
        LiveAction::ForwardMulti {
            targets,
            text,
            base_client_id,
        } => api
            .forward_multi_text(&targets, &text, base_client_id)
            .await
            .map(|_| serde_json::Value::Null),
        LiveAction::Block { uid } => api.set_block(&uid, true).await,
        LiveAction::SendPhoto {
            client_id: cid,
            thread_id,
            is_group,
            path,
            caption,
            group_layout_id,
            group_total,
            group_index,
        } => {
            let (jpeg, w, h) = encode_upload(&path)?;
            let up = api
                .upload_photo_best(
                    &thread_id,
                    is_group,
                    jpeg,
                    &format!("zalo-rust-{cid}.jpg"),
                    cid,
                )
                .await
                .map_err(|e| format!("upload: {e}"))?;
            let group = (group_total > 1 && group_layout_id != 0)
                .then_some((group_layout_id, group_total, group_index));
            api.send_photo(&thread_id, is_group, &up, w, h, &caption, group)
                .await
        }
        LiveAction::SendPhotoByUrl {
            client_id: cid,
            thread_id,
            is_group,
            info,
        } => {
            api.send_photo_by_url(&thread_id, is_group, cid, &info)
                .await
        }
        LiveAction::SendFile {
            client_id: cid,
            thread_id,
            is_group,
            path,
        } => {
            // Hash and upload from disk. `upload_async_file_path` sends 3 MiB
            // chunks, so the whole file is not held in memory.
            use md5::{Digest, Md5};
            if !crate::realtime::listening() {
                return Err("chưa kết nối realtime (cần để nhận file_done)".into());
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_owned();
            let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut hasher = Md5::new();
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = std::io::Read::read(&mut file, &mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            drop(file);
            let checksum = format!("{:x}", hasher.finalize());
            let (file_id, size) = api
                .upload_async_file_path(&thread_id, is_group, &path, &name, cid)
                .await
                .map_err(|e| format!("upload: {e}"))?;
            let url = crate::realtime::wait_file_url(&file_id, std::time::Duration::from_secs(90))
                .ok_or("máy chủ chưa báo file_done sau 90 s")?;
            api.send_async_file_msg(
                &thread_id, is_group, &file_id, &url, &name, size, &checksum, cid,
            )
            .await
        }
        LiveAction::SendSticker {
            client_id: cid,
            thread_id,
            is_group,
            sticker,
        } => api.send_sticker(&thread_id, is_group, &sticker, cid).await,
        LiveAction::SendGif {
            client_id: cid,
            thread_id,
            is_group,
            gif,
        } => api.send_gif(&thread_id, is_group, &gif, cid).await,
        LiveAction::SendVoice {
            client_id: cid,
            thread_id,
            is_group,
            bytes,
            file_name,
        } => {
            let use_chunks =
                bytes.len() as u64 > VOICE_DIRECT_MAX_BYTES && crate::realtime::listening();
            if use_chunks {
                let (file_id, immediate) = api
                    .upload_voice_chunks(&thread_id, is_group, &bytes, &file_name, cid)
                    .await
                    .map_err(|e| format!("voice upload: {e}"))?;
                let voice_url = if !immediate.is_empty() {
                    immediate
                } else {
                    crate::realtime::wait_file_url(&file_id, std::time::Duration::from_secs(90))
                        .ok_or("máy chủ chưa báo voice_done sau 90 s")?
                };
                api.forward_voice(&thread_id, is_group, cid, &voice_url, None)
                    .await
            } else {
                match api
                    .send_voice(&thread_id, is_group, bytes.clone(), &file_name, cid)
                    .await
                {
                    Ok(_) => Ok(serde_json::Value::Null),
                    Err(err) if crate::realtime::listening() => {
                        if let Ok((file_id, immediate)) = api
                            .upload_voice_chunks(&thread_id, is_group, &bytes, &file_name, cid)
                            .await
                        {
                            let voice_url = if !immediate.is_empty() {
                                Some(immediate)
                            } else {
                                crate::realtime::wait_file_url(
                                    &file_id,
                                    std::time::Duration::from_secs(60),
                                )
                            };
                            if let Some(url) = voice_url {
                                api.forward_voice(&thread_id, is_group, cid, &url, None)
                                    .await
                            } else {
                                Err(err)
                            }
                        } else {
                            Err(err)
                        }
                    }
                    Err(err) => Err(err),
                }
            }
        }
        LiveAction::SendVideo {
            client_id: cid,
            thread_id,
            is_group,
            path,
            duration_ms,
            width,
            height,
            thumb,
        } => {
            // Same socket gate as async files: the playable URL is `video_done`.
            if !crate::realtime::listening() {
                return Err("chưa kết nối realtime (cần để nhận video_done)".into());
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("video.mp4")
                .to_owned();
            let (file_id, file_size) = api
                .upload_video_file_path(&thread_id, is_group, &path, &name, cid)
                .await
                .map_err(|e| format!("upload: {e}"))?;
            let thumb_url = match thumb.as_ref().and_then(|p| std::fs::read(p).ok()) {
                Some(tb) if !tb.is_empty() => {
                    let primary = api
                        .upload_video_thumb(&thread_id, is_group, &tb, "thumb.jpg", cid)
                        .await
                        .unwrap_or_default();
                    if !primary.is_empty() {
                        primary
                    } else {
                        api.upload_thumb(cid, is_group, tb, "thumb.jpg")
                            .await
                            .unwrap_or_default()
                    }
                }
                _ => String::new(),
            };
            let url = crate::realtime::wait_file_url(&file_id, std::time::Duration::from_secs(120))
                .ok_or_else(|| "máy chủ chưa báo video_done sau 120 s".to_owned())?;
            let info = zalo_client::ops::VideoSendInfo {
                title: name,
                video_url: url,
                thumb_url,
                duration_ms,
                file_size,
                width,
                height,
            };
            api.forward_video(&thread_id, is_group, cid, &info).await
        }
        LiveAction::Seen { thread_id, record } => api.send_seen(&thread_id, &[record]).await,
        LiveAction::Delivered { thread_id, record } => {
            api.send_delivered(&thread_id, &[record]).await
        }
        LiveAction::Typing {
            thread_id,
            is_group,
        } => api.send_typing(&thread_id, is_group).await,
        LiveAction::MarkUnread { item } => api.add_unread_mark(&item).await,
        LiveAction::RemoveUnread {
            thread_id,
            is_group,
            ts,
        } => api.remove_unread_mark(&thread_id, is_group, ts).await,
        LiveAction::DeleteConv { item, client_id } => {
            api.delete_conversation(&item, client_id).await
        }
        LiveAction::RemoveFriend { uid } => api.remove_friend(&uid).await,
        LiveAction::UpdateAlias { uid, alias } => {
            if alias.trim().is_empty() {
                api.remove_alias(&uid).await
            } else {
                api.update_alias(&uid, alias.trim()).await
            }
        }
        LiveAction::ReportAbuse {
            target_id,
            is_group,
            reason,
            content,
        } => {
            api.report_abuse(&target_id, is_group, reason, &content)
                .await
        }
        LiveAction::PinBoard {
            thread_id,
            topic_id,
            item_type,
        } => api.pin_board_item(&thread_id, &topic_id, item_type).await,
        LiveAction::UnpinBoard {
            thread_id,
            topic_id,
            board_version,
        } => {
            api.unpin_board_item(&thread_id, &topic_id, board_version)
                .await
        }
        LiveAction::VotePoll {
            poll_id,
            option_ids,
        } => api.vote_poll(poll_id, &option_ids).await,
    }
    .map(|_| ())
    .map_err(|e| e.to_string())
}

static REPORTER: std::sync::Mutex<Option<futures::channel::mpsc::UnboundedSender<String>>> =
    std::sync::Mutex::new(None);

/// The view that shows bridge outcomes (errors, file sent).
pub fn set_reporter(tx: futures::channel::mpsc::UnboundedSender<String>) {
    if let Ok(mut r) = REPORTER.lock() {
        *r = Some(tx);
    }
}

fn report(text: String) {
    if let Ok(r) = REPORTER.lock() {
        if let Some(tx) = r.as_ref() {
            let _ = tx.unbounded_send(text);
        }
    }
}

/// First line, without HTML bodies.
fn short(err: &str) -> String {
    let line = err.lines().next().unwrap_or(err);
    let cut = line.find('<').unwrap_or(line.len());
    line[..cut].chars().take(160).collect()
}

/// Run one read/write call with a fresh client (session + `getLoginInfo`,
/// like zalo-gui's `spawn_api`). Blocking: background executor only.
///
/// # Errors
///
/// Returns a short message without a session or when `job` fails.
pub fn block_api<T, F, Fut>(dir: &std::path::Path, job: F) -> Result<T, String>
where
    F: FnOnce(ZaloApi) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let session = StoredSession::load(dir).map_err(|e| e.to_string())?;
    let Some(sek) = session.zpw_sek() else {
        return Err("chưa đăng nhập".into());
    };
    if !dir.join("device.json").is_file() {
        return Err("chưa đăng nhập".into());
    }
    let device = Device::load_or_create(dir).map_err(|e| e.to_string())?;
    let mut api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        ensure_service_hosts(&mut api, dir, &sek, false).await?;
        job(api).await
    })
    .map_err(|e| short(&e))
}

/// `/api/convlabel/get` on a user click (opening "Labels"). Blocking:
/// run it on a background executor.
///
/// # Errors
///
/// Returns a short message when there is no usable session or the call fails.
pub fn fetch_labels(dir: &std::path::Path) -> Result<Vec<zalo_client::ConversationLabel>, String> {
    let session = StoredSession::load(dir).map_err(|e| e.to_string())?;
    let Some(sek) = session.zpw_sek() else {
        return Err("session has no zpw_sek".into());
    };
    if !dir.join("device.json").is_file() {
        return Err("no device.json".into());
    }
    let device = Device::load_or_create(dir).map_err(|e| e.to_string())?;
    let mut api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let value = rt
        .block_on(async move {
            ensure_service_hosts(&mut api, dir, &sek, false)
                .await
                .map_err(|e| e)?;
            api.get_labels().await.map_err(|e| e.to_string())
        })?;
    if std::env::var_os("ZALO_GPUI_DEBUG_LABELS").is_some() {
        eprintln!("zalo-gpui labels raw: {value}");
    }
    Ok(zalo_client::ConversationLabel::list_from(&value))
}

/// Maps a [`crate::media::MsgKind`] to Zalo's numeric reaction `msgType`.
#[must_use]
pub fn msg_type_for(kind: crate::media::MsgKind) -> i64 {
    match kind {
        crate::media::MsgKind::Voice => MSG_VOICE,
        crate::media::MsgKind::File => MSG_FILE,
        _ => MSG_TEXT,
    }
}

/// `clientId` from unix milliseconds.
#[must_use]
pub fn client_id() -> u64 {
    crate::data::now_ms().max(1) as u64
}

/// True when `/api/message/parselink` returned real preview metadata (a title
/// distinct from the raw URL, a description, or a thumbnail), matching Zalo PC's
/// `if (s) { n.type = MSG_LINK_CLIENT; ... }` gate.
fn has_rich_link_metadata(parsed: Option<&serde_json::Value>, href: &str) -> bool {
    let Some(v) = parsed else {
        return false;
    };
    let preview = crate::links::from_parselink(v);
    let url_trim = href.trim();
    let has_real_title = !preview.title.trim().is_empty()
        && preview.title.trim() != url_trim
        && !preview.title.trim().starts_with("http://")
        && !preview.title.trim().starts_with("https://");
    let has_real_desc = !preview.desc.trim().is_empty()
        && preview.desc.trim() != url_trim
        && !preview.desc.trim().starts_with("http://")
        && !preview.desc.trim().starts_with("https://");
    let has_thumb = !preview.thumb.trim().is_empty()
        && !preview.thumb.contains("default")
        && !preview.thumb.contains("placeholder");
    has_real_title || has_real_desc || has_thumb
}

fn build_link_send_info(
    msg: &str,
    href: &str,
    parsed: Option<&serde_json::Value>,
) -> LinkSendInfo {
    let root = parsed
        .and_then(|v| v.get("data").filter(|d| d.is_object()).or(Some(v)))
        .filter(|v| v.is_object());
    let pick = |keys: &[&str]| -> String {
        root.and_then(|r| {
            keys.iter().find_map(|k| {
                r.get(*k)
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
        })
        .unwrap_or_default()
    };
    let host = href
        .trim()
        .strip_prefix("https://")
        .or_else(|| href.trim().strip_prefix("http://"))
        .and_then(|s| s.split('/').next())
        .unwrap_or("")
        .to_owned();
    let title = pick(&["title", "mediaTitle"]);
    let desc = pick(&["desc", "description"]);
    let thumb = pick(&["thumb", "thumbUrl", "ogImage"]);
    let src = {
        let s = pick(&["src", "domain"]);
        if s.is_empty() { host } else { s }
    };
    let media = root
        .and_then(|r| r.get("media"))
        .map(|m| {
            if let Some(s) = m.as_str() {
                s.to_owned()
            } else {
                m.to_string()
            }
        })
        .unwrap_or_else(|| "{}".to_owned());
    let link_type = root
        .and_then(|r| r.get("type").and_then(serde_json::Value::as_i64))
        .unwrap_or(0);
    LinkSendInfo {
        msg: msg.to_owned(),
        href: href.to_owned(),
        src,
        title: if title.is_empty() {
            href.to_owned()
        } else {
            title
        },
        desc,
        thumb,
        media,
        link_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_mute_constants_match_ops() {
        assert_eq!(PIN_ADD, 1);
        assert_eq!(PIN_REMOVE, 2);
        assert_eq!(SET_MUTE, 1);
        assert_eq!(UNSET_MUTE, 3);
        assert_eq!(ARCHIVE_ADD, 0);
        assert_eq!(ARCHIVE_REMOVE, 1);
        assert_eq!(MSG_TEXT, 1);
        assert!(!live_allowed("preview:1"));
        assert!(live_allowed("g_1"));
    }

    #[test]
    fn spawn_live_drops_preview_without_thread() {
        spawn_live(
            LiveAction::Send {
                thread_id: "preview:1".into(),
                is_group: false,
                text: "no".into(),
                quote: None,
                mentions: Vec::new(),
                language: "vi".into(),
                urgency: 0,
                client_id: 1,
            },
            PathBuf::from("/tmp/zalo-gpui-no-session"),
        );
        spawn_live(
            LiveAction::Block { uid: "1".into() },
            PathBuf::from("/tmp/zalo-gpui-no-session"),
        );
    }

    #[test]
    fn rich_link_metadata_gate_matches_zalo_pc() {
        assert!(!has_rich_link_metadata(None, "https://lza.lol"));
        assert!(!has_rich_link_metadata(
            Some(&serde_json::json!({})),
            "https://lza.lol"
        ));
        assert!(!has_rich_link_metadata(
            Some(&serde_json::json!({"data": {"title": "https://lza.lol", "desc": "", "thumb": ""}})),
            "https://lza.lol"
        ));
        assert!(has_rich_link_metadata(
            Some(&serde_json::json!({"data": {"title": "Example Domain", "desc": "", "thumb": ""}})),
            "https://example.com"
        ));
        assert!(has_rich_link_metadata(
            Some(&serde_json::json!({"data": {"title": "", "desc": "Useful page", "thumb": ""}})),
            "https://example.com"
        ));
        assert!(has_rich_link_metadata(
            Some(&serde_json::json!({"data": {"title": "", "desc": "", "thumb": "https://cdn.example/og.jpg"}})),
            "https://example.com"
        ));
    }
}
