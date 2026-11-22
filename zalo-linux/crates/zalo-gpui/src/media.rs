//! Classify stored bodies (photo/file/link/sticker/…) and wrap bubble text.
//!
//! Photos load from `~/.cache/zalo-rust/photos` (same keys as `zalo-gui`).
//! Missing files are fetched from the CDN by [`crate::fetch`] when a thread
//! is opened (budget 4, newest first), never for preview fixtures or tests.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::RenderImage;
use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frame};

use crate::color::str_vi;

/// CSS-like bubble cap (egui `max_w` 360).
pub const BUBBLE_MAX: f32 = 360.0;
/// `.card` pad 12 on each side.
pub const BUBBLE_PAD: f32 = 12.0;
/// Text / media width inside the card.
pub const BUBBLE_INNER: f32 = BUBBLE_MAX - BUBBLE_PAD * 2.0;
/// Photo longest side in the bubble.
pub const PHOTO_MAX_W: f32 = 280.0;
/// Photo height cap.
pub const PHOTO_MAX_H: f32 = 320.0;
/// Sticker drawn without a bubble frame.
pub const STICKER_SIZE: f32 = 120.0;

/// Kind of one stored message body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MsgKind {
    /// Plain / quoted text.
    #[default]
    Text,
    /// `chat.photo`.
    Photo,
    /// `chat.sticker`.
    Sticker,
    /// `chat.gif`.
    Gif,
    /// `share.file`.
    File,
    /// URL / `chat.link`.
    Link,
    /// `recommened.calltime`.
    Call,
    /// `recommened.misscall`.
    MissedCall,
    /// `chat.voice`.
    Voice,
    /// `chat.video.msg`.
    Video,
    /// `recommened.user` contact card.
    Card,
    /// `group.poll`.
    Poll,
    /// `chat.location.new`.
    Location,
    /// `chat.doodle` (a drawn picture, shown like a photo).
    Doodle,
    /// `chat.ecard`.
    Ecard,
}

/// One choice on a `group.poll` card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PollChoice {
    /// `option_id`.
    pub id: i64,
    /// Label (`content`).
    pub content: String,
    /// `total_vote`, or the length of `voters`.
    pub votes: i64,
    /// This account already chose it (`voted`).
    pub voted: bool,
}

/// Poll fields carried by a `group.poll` message or `/api/poll/detail`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PollBody {
    /// `pollId` / `poll_id`. `0` when the message has none.
    pub id: i64,
    /// Question.
    pub question: String,
    /// `create`, `share`, `vote`, `update`, or `close`.
    pub action: String,
    /// `status != 0`, or action `close`.
    pub closed: bool,
    /// Choices. Empty until detail is loaded.
    pub options: Vec<PollChoice>,
}

/// Fields parsed from `messages.body` + `attachments.data`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaMeta {
    /// Display kind.
    pub kind: MsgKind,
    /// Cached image bytes on disk, when present.
    pub path: Option<PathBuf>,
    /// Declared pixel width.
    pub width: u32,
    /// Declared pixel height.
    pub height: u32,
    /// File name or link host.
    pub title: String,
    /// File size in bytes.
    pub size: u64,
    /// http(s) href for a link card.
    pub href: String,
    /// Remote thumb (or HD) URL of a photo / sticker / GIF. Empty when none.
    pub url: String,
    /// Quoted message sender name (stored inside `attachments.data["quote"]`).
    pub quote_sender: String,
    /// Quoted message snippet (stored inside `attachments.data["quote"]`).
    pub quote_text: String,
    /// Quoted server `globalMsgId` / `msgId`.
    pub quote_global_id: Option<i64>,
    /// Quoted client `cliMsgId`.
    pub quote_cli_id: Option<i64>,
    /// `group.poll` id. `0` when this row is not a poll.
    pub poll_id: i64,
    /// `create` / `share` / `vote` / `update` / `close`.
    pub poll_action: String,
    /// Poll is closed.
    pub poll_closed: bool,
    /// Choices already known (the message or a later detail fetch).
    pub poll_options: Vec<PollChoice>,
    /// Stored `group_layout_id` for one cell of a multi-photo send. `0` if none.
    pub group_layout_id: i64,
}

/// FNV-1a of a photo URL — same as `zalo-gui` `url_key`.
#[must_use]
pub fn url_key(url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

static MEDIA_ROOT: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Settings › Data Management › "Storage Location": move where photos and
/// thumbnails live (`None` = `~/.cache/zalo-rust`).
pub fn set_media_root(root: Option<PathBuf>) {
    if let Ok(mut r) = MEDIA_ROOT.write() {
        *r = root;
    }
}

/// Folder holding `photos/` and `thumbs/`.
#[must_use]
pub fn media_root() -> Option<PathBuf> {
    if let Some(r) = MEDIA_ROOT.read().ok().and_then(|r| r.clone()) {
        return Some(r);
    }
    Some(
        zalo_core::AppPaths::from_env("zalo-rust")
            .ok()?
            .cache_dir()
            .to_path_buf(),
    )
}

/// `<media root>/photos`.
#[must_use]
pub fn photo_cache_dir() -> Option<PathBuf> {
    let dir = media_root()?.join("photos");
    dir.is_dir().then_some(dir)
}

fn cached_url(url: &str) -> Option<PathBuf> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return None;
    }
    let path = photo_cache_dir()?.join(url_key(url));
    path.is_file().then_some(path)
}

/// Parse attachment JSON and/or the stored body marker.
#[must_use]
pub fn classify(body: &str, attach_json: Option<&str>) -> MediaMeta {
    let mut meta = MediaMeta::default();
    if let Some(json) = attach_json {
        fill_from_attach(&mut meta, json);
    }
    if meta.kind == MsgKind::Text {
        fill_from_body(&mut meta, body);
    } else if meta.title.is_empty() {
        fill_title_from_body(&mut meta, body);
    }
    meta
}

fn fill_from_attach(meta: &mut MediaMeta, json: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return;
    };
    if let Some(q) = value.get("quote").filter(|v| v.is_object()) {
        meta.quote_sender = q
            .get("sender")
            .or_else(|| q.get("fromD"))
            .or_else(|| q.get("ownerId"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        meta.quote_text = q
            .get("text")
            .or_else(|| q.get("msg"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        meta.quote_global_id = q
            .get("gid")
            .or_else(|| q.get("globalMsgId"))
            .and_then(json_i64);
        meta.quote_cli_id = q
            .get("cid")
            .or_else(|| q.get("cliMsgId"))
            .and_then(json_i64);
    }
    let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if kind.is_empty() && value.get("msgType").and_then(|v| v.as_str()).is_some() {
        // Phone → PC sync stores the server message (`msgType` + `content`),
        // not the compact `kind` object the live socket writes.
        fill_from_server_message(meta, &value);
        return;
    }
    let num = |key: &str| -> u64 {
        value
            .get(key)
            .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|n| n.max(0) as u64)))
            .unwrap_or(0)
    };
    let text = |key: &str| -> String {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned()
    };
    match kind {
        "photo" => meta.kind = MsgKind::Photo,
        "sticker" => meta.kind = MsgKind::Sticker,
        "gif" => meta.kind = MsgKind::Gif,
        "file" => meta.kind = MsgKind::File,
        "voice" => meta.kind = MsgKind::Voice,
        "video" => meta.kind = MsgKind::Video,
        "doodle" => meta.kind = MsgKind::Doodle,
        "location" => {
            meta.kind = MsgKind::Location;
            meta.title = text("title");
            let href = text("href");
            if href.starts_with("http://") || href.starts_with("https://") {
                meta.href = href;
            }
            meta.url = text("thumb");
            if meta.path.is_none() {
                meta.path = cached_url(&meta.url);
            }
            return;
        }
        "ecard" => {
            meta.kind = MsgKind::Ecard;
            meta.title = text("title");
            let href = text("href");
            if href.starts_with("http://") || href.starts_with("https://") {
                meta.href = href;
            }
            meta.url = text("thumb");
            return;
        }
        "card" => {
            meta.kind = MsgKind::Card;
            meta.title = text("title");
            meta.url = text("thumb");
            if meta.url.is_empty() {
                meta.url = text("hd");
            }
            return;
        }
        "poll" => {
            meta.kind = MsgKind::Poll;
            if let Some(body) = poll_from_value(&value) {
                apply_poll(meta, &body);
            }
            if meta.title.is_empty() {
                meta.title = text("question");
            }
            if meta.title.is_empty() {
                meta.title = text("title");
            }
            if meta.poll_action.is_empty() {
                meta.poll_action = text("action");
            }
            return;
        }
        _ => return,
    }
    // Voice / video file (`size` is the length in ms for those two).
    let href = text("href");
    if is_http_url(&href) {
        meta.href = href;
    }
    meta.width = num("width") as u32;
    meta.height = num("height") as u32;
    meta.size = num("size");
    meta.group_layout_id = value
        .get("group_layout_id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if meta.kind == MsgKind::File {
        meta.title = text("name");
    }
    if let Some(local) = value.get("local").and_then(|v| v.as_str()) {
        let path = PathBuf::from(local);
        if path.is_file() {
            meta.path = Some(path);
        }
    }
    let thumb = text("thumb");
    let hd = text("hd");
    // A GIF's `thumb` is often a still PNG; the moving file is `hd` / `.gif`.
    // A photo's `thumb` is often a low-res 1:1 square crop from Zalo CDN,
    // whereas `hd` / `href` is the full-resolution uncropped image.
    meta.url = if meta.kind == MsgKind::Gif {
        [hd.as_str(), thumb.as_str()]
            .into_iter()
            .find(|u| is_http_url(u) && url_path_ends_gif(u))
            .or_else(|| [hd.as_str(), thumb.as_str()].into_iter().find(|u| is_http_url(u)))
            .unwrap_or("")
            .to_owned()
    } else if matches!(meta.kind, MsgKind::Photo | MsgKind::Doodle) {
        pick_full_photo_url("", &hd, &meta.href, &thumb)
    } else {
        [thumb.as_str(), hd.as_str()]
            .into_iter()
            .find(|u| is_http_url(u))
            .unwrap_or("")
            .to_owned()
    };
    if meta.path.is_none() {
        meta.path = cached_url(&meta.url);
    }
}

/// True when `url` is an http(s) URL.
#[must_use]
pub fn is_http_url(url: &str) -> bool {
    let t = url.trim();
    t.starts_with("https://") || t.starts_with("http://")
}

/// True when a Zalo CDN photo URL points to a small cropped thumbnail rather
/// than the full-resolution original image.
#[must_use]
pub fn is_thumb_photo_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("/thumb/") || lower.contains("/t/") || lower.contains("_thumb.")
}

/// Choose the highest-resolution uncropped photo URL from Zalo's attachment
/// fields (`oriUrl`, `hd`, `href`, `thumb`).
///
/// Zalo CDN's `thumb` is frequently a blurry 240×240 square crop, while
/// `oriUrl`, `hd`, and `href` carry the full-resolution uncropped image
/// (including `.jxl`, which [`decode_rgba`] decodes via `zjxl`). In Sync 2
/// payloads, `content.href` sometimes mirrors `content.thumb` while
/// `params.hd` carries the real HD `.jxl` URL — comparing against `thumb`
/// ensures `params.hd` wins in that case.
#[must_use]
pub fn pick_full_photo_url(ori: &str, hd: &str, href: &str, thumb: &str) -> String {
    let thumb = thumb.trim();
    let full_candidates = [ori.trim(), hd.trim(), href.trim()];
    let is_full = |u: &&str| is_http_url(u) && *u != thumb && !is_thumb_photo_url(u);
    if let Some(best) = full_candidates
        .iter()
        .find(|u| is_full(u) && !u.to_ascii_lowercase().contains(".jxl"))
        .or_else(|| full_candidates.iter().find(|u| is_full(u)))
    {
        return (*best).to_owned();
    }
    if let Some(best) = full_candidates
        .iter()
        .find(|u| is_http_url(u) && **u != thumb)
    {
        return (*best).to_owned();
    }
    [href.trim(), hd.trim(), ori.trim(), thumb]
        .into_iter()
        .find(|u| is_http_url(u))
        .unwrap_or("")
        .to_owned()
}

/// Attachment written by Sync 2: the server message (`msgType`, `content`,
/// `content.params` as a JSON string).
fn fill_from_server_message(meta: &mut MediaMeta, value: &serde_json::Value) {
    let msg_type = value
        .get("msgType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let content = nested_object(value.get("content"));
    let params = nested_object(content.get("params"));
    let action = content
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let https = |v: Option<&serde_json::Value>| -> String {
        v.and_then(|item| item.as_str())
            .filter(|url| is_http_url(url))
            .unwrap_or("")
            .to_owned()
    };
    let text = |obj: &serde_json::Value, key: &str| -> String {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let num = |obj: &serde_json::Value, key: &str| -> u32 {
        obj.get(key)
            .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|n| n.max(0) as u64)))
            .unwrap_or(0) as u32
    };
    let picture = |content: &serde_json::Value, params: &serde_json::Value| -> String {
        let thumb = {
            let t = https(content.get("thumb"));
            if t.is_empty() {
                https(params.get("thumb"))
            } else {
                t
            }
        };
        pick_full_photo_url(
            &https(params.get("oriUrl")),
            &https(params.get("hd")),
            &https(content.get("href")),
            &thumb,
        )
    };
    match msg_type {
        "chat.photo" => {
            meta.kind = MsgKind::Photo;
            meta.url = picture(&content, &params);
            meta.width = num(&params, "width");
            meta.height = num(&params, "height");
        }
        "chat.gif" => {
            meta.kind = MsgKind::Gif;
            let href = https(content.get("href"));
            let hd = https(params.get("hd"));
            meta.url = [href, hd, https(content.get("thumb"))]
                .into_iter()
                .find(|url| is_http_url(url))
                .unwrap_or_default();
            meta.width = num(&params, "width");
            meta.height = num(&params, "height");
        }
        "chat.sticker" => {
            meta.kind = MsgKind::Sticker;
            if let Some(sticker) = zalo_client::StickerRef::from_value(&content)
                .or_else(|| zalo_client::StickerRef::from_value(&params))
            {
                meta.url = sticker.image_url();
                meta.width = 130;
                meta.height = 130;
            }
            if meta.url.is_empty() {
                meta.url = picture(&content, &params);
            }
        }
        "chat.voice" => {
            meta.kind = MsgKind::Voice;
            meta.href = https(content.get("href"));
            meta.size = u64::from(num(&params, "duration"));
        }
        "chat.video.msg" => {
            meta.kind = MsgKind::Video;
            meta.href = https(content.get("href"));
            meta.url = https(content.get("thumb"));
            meta.size = u64::from(num(&params, "duration"));
            meta.width = num(&params, "video_width").max(num(&params, "width"));
            meta.height = num(&params, "video_height").max(num(&params, "height"));
        }
        "share.file" => {
            meta.kind = MsgKind::File;
            meta.title = text(&content, "title");
            meta.href = https(content.get("href"));
            let size = content
                .get("fileSize")
                .or_else(|| params.get("fileSize"))
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_i64().map(|n| n.max(0) as u64))
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .unwrap_or(0);
            meta.size = size;
        }
        "chat.doodle" => {
            meta.kind = MsgKind::Doodle;
            meta.url = https(content.get("href"));
            if meta.url.is_empty() {
                meta.url = picture(&content, &params);
            }
            meta.width = num(&params, "width");
            meta.height = num(&params, "height");
        }
        "chat.location.new" => {
            meta.kind = MsgKind::Location;
            meta.title = text(&content, "title");
            meta.href = text(&content, "href");
            meta.url = https(content.get("thumb"));
        }
        "chat.ecard" => {
            meta.kind = MsgKind::Ecard;
            meta.title = text(&content, "title");
            meta.href = text(&content, "href");
            meta.url = https(content.get("thumb"));
        }
        "group.poll" => {
            let body = poll_from_message(&content);
            apply_poll(meta, &body);
        }
        "chat.recommended" | "chat.link" => match action {
            "recommened.misscall" => meta.kind = MsgKind::MissedCall,
            "recommened.calltime" => meta.kind = MsgKind::Call,
            "recommened.user" | "recommened.vip" => {
                meta.kind = MsgKind::Card;
                meta.title = text(&content, "title");
                meta.url = https(content.get("thumb"));
                if meta.url.is_empty() {
                    meta.url = https(params.get("avatar"));
                }
            }
            _ => {
                let href = text(&content, "href");
                if href.starts_with("http://") || href.starts_with("https://") {
                    meta.kind = MsgKind::Link;
                    meta.href = href;
                    meta.title = text(&content, "title");
                    meta.url = https(content.get("thumb"));
                }
            }
        },
        _ => {}
    }
    if meta.path.is_none() && !meta.url.is_empty() {
        meta.path = cached_url(&meta.url);
    }
}

fn nested_object(value: Option<&serde_json::Value>) -> serde_json::Value {
    match value {
        Some(serde_json::Value::String(text)) => {
            serde_json::from_str(text).unwrap_or(serde_json::Value::Null)
        }
        Some(other) => other.clone(),
        None => serde_json::Value::Null,
    }
}

/// Cache file for a remote media URL (`~/.cache/zalo-rust/photos/<fnv>`),
/// creating the directory. `None` for non-http(s) URLs.
#[must_use]
pub fn cache_path_for(url: &str) -> Option<PathBuf> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return None;
    }
    let dir = media_root()?.join("photos");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(url_key(url)))
}

/// Resolves an already-downloaded local file for a message from `media_path`,
/// then cached `media_url`, then cached `href`.
#[must_use]
pub fn local_media_file(
    media_path: Option<&Path>,
    media_url: &str,
    href: &str,
) -> Option<PathBuf> {
    media_path
        .filter(|p| p.is_file())
        .map(Path::to_path_buf)
        .or_else(|| cache_path_for(media_url).filter(|p| p.is_file()))
        .or_else(|| cache_path_for(href).filter(|p| p.is_file()))
}

/// Look for a received file by name in the usual download folders
/// (`~/Downloads/Zalo Received Files`, `~/Downloads`, XDG download dir).
#[must_use]
pub fn find_downloaded(name: &str) -> Option<PathBuf> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    let name = name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return None;
    }
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Option<PathBuf>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let now = Instant::now();
    if let Ok(map) = cache.lock() {
        if let Some((at, hit)) = map.get(name) {
            if now.duration_since(*at) < Duration::from_secs(3) {
                return hit.clone();
            }
        }
    }
    let found = (|| {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        let mut dirs = vec![
            home.join("Downloads").join("Zalo Received Files"),
            home.join("Downloads"),
            home.join("Tải xuống"),
        ];
        if let Some(xdg) = std::env::var_os("XDG_DOWNLOAD_DIR") {
            dirs.insert(0, PathBuf::from(xdg));
        }
        dirs.into_iter()
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    })();
    if let Ok(mut map) = cache.lock() {
        if map.len() > 256 {
            map.retain(|_, (at, _)| now.duration_since(*at) < Duration::from_secs(3));
        }
        map.insert(name.to_owned(), (now, found.clone()));
    }
    found
}

/// Group-call system bodies that `zalo-gui` stored as the raw action name.
#[must_use]
pub fn is_group_call_body(body: &str) -> bool {
    matches!(
        body.trim(),
        "sendGroupCallMessage" | "recommened.groupcall" | "chat.recommended.groupcall"
    )
}

/// Readable sender label. Unknown senders were saved with their id as the
/// name (e.g. a 32-char `MVHFH…` zid); show "Zalo User" instead.
#[must_use]
pub fn sender_label(uid: &str, name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name == uid || looks_like_id(name) {
        return str_vi("STR_ZALO_USER", "Người dùng Zalo").to_owned();
    }
    name.to_owned()
}

fn looks_like_id(text: &str) -> bool {
    text.len() >= 16
        && !text.contains(' ')
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase() || c == '_' || c == '-')
}

fn fill_from_body(meta: &mut MediaMeta, body: &str) {
    let trimmed = body.trim();
    if let Some(name) = trimmed.strip_prefix("[File]") {
        meta.kind = MsgKind::File;
        meta.title = name.trim().to_owned();
        return;
    }
    if let Some(rest) = trimmed.strip_prefix("[Danh thiếp]") {
        meta.kind = MsgKind::Card;
        meta.title = rest.trim().lines().next().unwrap_or("").trim().to_owned();
        return;
    }
    if let Some(question) = trimmed.strip_prefix("[Bình chọn]") {
        meta.kind = MsgKind::Poll;
        meta.title = question.trim().to_owned();
        return;
    }
    if let Some(rest) = trimmed.strip_prefix("[Vị trí]") {
        meta.kind = MsgKind::Location;
        meta.title = rest.trim().to_owned();
        return;
    }
    if trimmed == "[Vẽ hình]" || trimmed == "[Hình vẽ]" {
        meta.kind = MsgKind::Doodle;
        return;
    }
    if let Some(rest) = trimmed.strip_prefix("[Thiệp]") {
        meta.kind = MsgKind::Ecard;
        meta.title = rest.trim().to_owned();
        return;
    }
    match trimmed {
        "[Hình ảnh]" => meta.kind = MsgKind::Photo,
        "[Sticker]" => meta.kind = MsgKind::Sticker,
        "[GIF]" => meta.kind = MsgKind::Gif,
        "[Cuộc gọi nhỡ]" | "Cuộc gọi nhỡ" => meta.kind = MsgKind::MissedCall,
        "[Cuộc gọi]" | "Cuộc gọi" | "Cuộc gọi đến" | "Cuộc gọi đi" | "Cuộc gọi thoại đến"
        | "Cuộc gọi thoại đi" => meta.kind = MsgKind::Call,
        "[Tin nhắn thoại]" | "[Voice]" => meta.kind = MsgKind::Voice,
        "[Video]" => meta.kind = MsgKind::Video,
        _ if is_group_call_body(trimmed) => {
            meta.kind = MsgKind::Call;
            meta.title = "Cuộc gọi nhóm".to_owned();
        }
        _ => {
            if let Some(url) = first_link(trimmed) {
                if trimmed == url
                    || trimmed.starts_with("http://")
                    || trimmed.starts_with("https://")
                {
                    meta.kind = MsgKind::Link;
                    meta.href = url;
                    meta.title = host_of(&meta.href).to_owned();
                }
            }
        }
    }
}

fn fill_title_from_body(meta: &mut MediaMeta, body: &str) {
    let trimmed = body.trim();
    if meta.kind == MsgKind::File {
        if let Some(name) = trimmed.strip_prefix("[File]") {
            if meta.title.is_empty() {
                meta.title = name.trim().to_owned();
            }
        }
    }
}

/// First http(s) token in `text`.
#[must_use]
pub fn first_link(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|w| w.starts_with("https://") || w.starts_with("http://"))
        .map(|w| {
            w.trim_end_matches(['.', ',', ')', ']', '>', '"', '\'', ';', '!', '?'])
                .to_owned()
        })
        .filter(|w| w.len() > "https://".len())
}

fn url_path_ends_gif(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase()
        .ends_with(".gif")
}

/// Host of a URL, without scheme.
#[must_use]
pub fn host_of(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

fn is_latin_char(ch: char) -> bool {
    matches!(ch as u32, 0x0000..=0x02FF | 0x1E00..=0x1EFF)
}

/// Proportional advance estimate at the 15 px bubble size (Segoe UI / Inter).
/// Vietnamese diacritics (`U+0080..=U+02FF`, `U+1E00..=U+1EFF`) are Latin
/// glyphs, NOT CJK full-width cells; treating them as 15 px caused premature
/// wrapping inside chat bubbles with large empty space on the right.
fn char_px(ch: char) -> f32 {
    match ch {
        '\t' => 28.0,
        ' ' => 4.5,
        '.' | ',' | ':' | ';' | '!' | '|' | '\'' | '`' => 4.6,
        'i' | 'l' | 'j' | 'I' | 'í' | 'ì' | 'ỉ' | 'ĩ' | 'ị' | 'Í' | 'Ì' | 'Ỉ' | 'Ĩ' | 'Ị' => 5.0,
        'f' | 'r' | 't' | '(' | ')' | '[' | ']' | '/' | '\\' | '-' => 5.8,
        'm' | 'w' => 12.0,
        'M' | 'W' => 13.8,
        c if c.is_ascii_uppercase() => 10.0,
        c if c.is_ascii_digit() => 8.2,
        c if c.is_ascii_lowercase() => 8.0,
        c if c.is_ascii() => 8.4,
        c if is_latin_char(c) => {
            if c.is_uppercase() {
                10.0
            } else {
                8.0
            }
        }
        _ => 15.5,
    }
}

/// Advance at the 13 px settings-note size (scaled proportionally from 15 px).
fn note_char_px(ch: char) -> f32 {
    if ch == '\t' {
        return 28.0;
    }
    char_px(ch) * (13.0 / 15.0)
}

/// Insert newlines so a long token cannot blow the bubble width.
#[must_use]
pub fn wrap_bubble_text(text: &str, max_px: f32) -> String {
    wrap_with(text, max_px, char_px)
}

/// Wrap text at an arbitrary pixel font size and optional semibold weight.
#[must_use]
pub fn wrap_text_at(text: &str, max_px: f32, font_px: f32, semibold: bool) -> String {
    let scale = (font_px / 15.0).max(0.5) * if semibold { 1.07 } else { 1.0 };
    wrap_with(text, max_px / scale, char_px)
}

/// Wrap a settings note. Wider per Latin letter than [`wrap_bubble_text`].
#[must_use]
pub fn wrap_ui_note(text: &str, max_px: f32) -> String {
    wrap_with(text, max_px, note_char_px)
}

fn wrap_with(text: &str, max_px: f32, metric: fn(char) -> f32) -> String {
    let max_px = max_px.max(48.0);
    let mut out = String::with_capacity(text.len() + 8);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        wrap_line(line, max_px, &mut out, metric);
    }
    out
}

fn wrap_line(line: &str, max_px: f32, out: &mut String, metric: fn(char) -> f32) {
    let mut line_w = 0.0_f32;
    for piece in line.split_inclusive(' ') {
        let trimmed = piece.trim_end_matches(' ');
        let word_w: f32 = trimmed.chars().map(metric).sum();
        if line_w > 0.0 && line_w + word_w > max_px {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
            line_w = 0.0;
            let piece = piece.trim_start();
            if piece.is_empty() {
                continue;
            }
            push_token(piece, max_px, out, &mut line_w, metric);
        } else {
            push_token(piece, max_px, out, &mut line_w, metric);
        }
    }
}

fn is_token_break(ch: char) -> bool {
    matches!(ch, '/' | '_' | '-' | '?' | '&' | '=' | '.' | ':' | ',' | ';' | '—' | '–')
}

fn push_token(
    token: &str,
    max_px: f32,
    out: &mut String,
    line_w: &mut f32,
    metric: fn(char) -> f32,
) {
    let ww: f32 = token.chars().map(metric).sum();
    if *line_w + ww <= max_px {
        out.push_str(token);
        *line_w += ww;
        return;
    }
    let chars: Vec<char> = token.chars().collect();
    let mut idx = 0;
    while idx < chars.len() {
        let mut end = idx;
        let mut w_acc = *line_w;
        while end < chars.len() {
            let cw = metric(chars[end]);
            if w_acc + cw > max_px && w_acc > 0.0 {
                break;
            }
            w_acc += cw;
            end += 1;
        }
        if end == idx {
            end = (idx + 1).min(chars.len());
        }
        if end < chars.len() {
            // Prefer breaking right after a natural separator (`_`, `/`, `-`, `.`, …)
            // in the second half of the chunk so extensions like `.zip` do not split
            // into `.zi\np`.
            let min_split = idx + ((end - idx) / 2).max(1);
            if let Some(pos) = (min_split..end).rev().find(|&k| is_token_break(chars[k])) {
                let break_at = if chars[pos] == '.' && end - pos <= 5 {
                    pos
                } else {
                    pos + 1
                };
                if break_at > idx {
                    end = break_at;
                }
            }
        }
        for &ch in &chars[idx..end] {
            if *line_w == 0.0 && ch == ' ' {
                continue;
            }
            out.push(ch);
            *line_w += metric(ch);
        }
        idx = end;
        if idx < chars.len() {
            out.push('\n');
            *line_w = 0.0;
        }
    }
}

/// Definite width of a wrapped string, capped at [`BUBBLE_INNER`].
#[must_use]
#[allow(dead_code)]
pub fn text_box_w(wrapped: &str) -> f32 {
    let mut max = 24.0_f32;
    for line in wrapped.lines() {
        max = max.max(line.chars().map(char_px).sum());
    }
    max.clamp(24.0, BUBBLE_INNER)
}

/// Fitted photo size inside the bubble.
#[must_use]
pub fn photo_size(width: u32, height: u32) -> (f32, f32) {
    if width == 0 || height == 0 {
        return (180.0, 120.0);
    }
    let aspect = width as f32 / height as f32;
    let mut w = PHOTO_MAX_W.min(width as f32);
    let mut h = w / aspect;
    if h > PHOTO_MAX_H {
        h = PHOTO_MAX_H;
        w = h * aspect;
    }
    (w.max(48.0), h.max(48.0))
}

/// How many animation frames a sticker or GIF bubble will keep.
const MAX_ANIM_FRAMES: usize = 32;

/// Decode a cache file (no extension) for a bubble whose longest side is
/// `max_edge` logical px. Decodes at 2× for HiDPI (1.35–2.0 scale).
///
/// GIF and animated WebP stay multi-frame so GPUI's `img` advances them.
/// A flattened PNG cache is only used for still images: it would freeze a
/// sticker on frame 0. Zalo CDN thumbs are mostly JPEG XL (`/gr/jxl/`); the
/// `image` crate cannot read them, so JXL goes through `zjxl`. GPUI
/// `RenderImage` frames are BGRA.
#[must_use]
pub fn decode_thumb(path: &Path, max_edge: u32) -> Option<Arc<RenderImage>> {
    let edge = max_edge.saturating_mul(2);
    let bytes = std::fs::read(path).ok()?;
    if let Some(anim) = decode_animated(&bytes, edge) {
        return Some(anim);
    }
    // Small PNG next to the photo cache: JXL decode happens once per photo.
    let thumb = thumb_path(path, edge);
    if let Some(rgba) = thumb
        .as_ref()
        .and_then(|t| std::fs::read(t).ok())
        .and_then(|b| image::load_from_memory_with_format(&b, image::ImageFormat::Png).ok())
        .map(|img| img.into_rgba8())
    {
        return Some(Arc::new(RenderImage::new([Frame::new(rgba_to_bgra(rgba))])));
    }
    let rgba = decode_rgba(&bytes, edge)?;
    if let Some(t) = thumb {
        let mut png = Vec::new();
        if rgba
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .is_ok()
        {
            let _ = std::fs::write(t, png);
        }
    }
    Some(Arc::new(RenderImage::new([Frame::new(rgba_to_bgra(rgba))])))
}

/// GIF / animated WebP → a multi-frame `RenderImage`. `None` for still images
/// so the caller can use the single-frame path (and its PNG cache).
fn decode_animated(bytes: &[u8], max_edge: u32) -> Option<Arc<RenderImage>> {
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let decoder = GifDecoder::new(Cursor::new(bytes)).ok()?;
        return frames_to_render(decoder.into_frames(), max_edge);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let decoder = WebPDecoder::new(Cursor::new(bytes)).ok()?;
        if !decoder.has_animation() {
            return None;
        }
        return frames_to_render(decoder.into_frames(), max_edge);
    }
    None
}

fn frames_to_render(frames: image::Frames<'_>, max_edge: u32) -> Option<Arc<RenderImage>> {
    let mut out: Vec<Frame> = Vec::new();
    for frame in frames {
        if out.len() >= MAX_ANIM_FRAMES {
            break;
        }
        let Ok(frame) = frame else {
            continue;
        };
        let (num, den) = frame.delay().numer_denom_ms();
        let ms = if den == 0 {
            100
        } else {
            (u64::from(num) / u64::from(den).max(1)).max(20) as u32
        };
        let rgba = frame.into_buffer();
        let image = image::DynamicImage::ImageRgba8(rgba);
        let image = if image.width() > max_edge || image.height() > max_edge {
            image.resize(max_edge, max_edge, image::imageops::FilterType::Triangle)
        } else {
            image
        };
        let delay = image::Delay::from_numer_denom_ms(ms, 1);
        out.push(Frame::from_parts(
            rgba_to_bgra(image.into_rgba8()),
            0,
            0,
            delay,
        ));
    }
    if out.len() < 2 {
        return None;
    }
    Some(Arc::new(RenderImage::new(out)))
}

fn apply_poll(meta: &mut MediaMeta, body: &PollBody) {
    meta.kind = MsgKind::Poll;
    meta.poll_id = body.id;
    meta.poll_action = body.action.clone();
    meta.poll_closed = body.closed;
    meta.poll_options = body.options.clone();
    if !body.question.is_empty() {
        meta.title = body.question.clone();
    }
}

fn json_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn json_text(value: &serde_json::Value, key: &str) -> String {
    match value.get(key) {
        Some(serde_json::Value::String(s)) => s.trim().to_owned(),
        Some(other) => other.as_i64().map(|n| n.to_string()).unwrap_or_default(),
        None => String::new(),
    }
}

fn has_poll(value: &serde_json::Value) -> bool {
    value.get("poll_id").is_some()
        || value.get("pollId").is_some()
        || value.get("question").is_some()
        || value
            .get("options")
            .and_then(serde_json::Value::as_array)
            .is_some()
}

/// `group.poll` content: `action` plus `params` (JSON string or object)
/// with `pollId` and `question`.
#[must_use]
pub fn poll_from_message(content: &serde_json::Value) -> PollBody {
    let action = json_text(content, "action");
    let params = match content.get("params") {
        Some(serde_json::Value::String(text)) => {
            serde_json::from_str(text).unwrap_or(serde_json::Value::Null)
        }
        Some(other) => other.clone(),
        None => serde_json::Value::Null,
    };
    let mut body = poll_from_value(&params)
        .or_else(|| poll_from_value(content))
        .unwrap_or_default();
    if body.action.is_empty() {
        body.action = action;
    }
    if body.action == "close" {
        body.closed = true;
    }
    body
}

/// `/api/poll/detail` or a poll object (`data` may be nested or a JSON string).
#[must_use]
pub fn poll_from_value(value: &serde_json::Value) -> Option<PollBody> {
    let mut node = value.clone();
    for _ in 0..4 {
        if has_poll(&node) {
            break;
        }
        match node.get("data") {
            Some(serde_json::Value::String(text)) => {
                node = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
            }
            Some(inner) if inner.is_object() => node = inner.clone(),
            _ => break,
        }
    }
    if !has_poll(&node) {
        return None;
    }
    let id = node
        .get("poll_id")
        .or_else(|| node.get("pollId"))
        .and_then(json_i64)
        .unwrap_or(0);
    let question = {
        let q = json_text(&node, "question");
        if q.is_empty() {
            json_text(&node, "title")
        } else {
            q
        }
    };
    let action = json_text(&node, "action");
    let status = node.get("status").and_then(json_i64).unwrap_or(0);
    let options: Vec<PollChoice> = node
        .get("options")
        .and_then(serde_json::Value::as_array)
        .map(|list| list.iter().filter_map(poll_choice).collect())
        .unwrap_or_default();
    if id == 0 && question.is_empty() && options.is_empty() {
        return None;
    }
    Some(PollBody {
        id,
        question,
        closed: status != 0 || action == "close",
        action,
        options,
    })
}

fn poll_choice(value: &serde_json::Value) -> Option<PollChoice> {
    let id = value
        .get("option_id")
        .or_else(|| value.get("optionId"))
        .or_else(|| value.get("id"))
        .and_then(json_i64)
        .unwrap_or(0);
    let content = {
        let c = json_text(value, "content");
        if c.is_empty() {
            json_text(value, "text")
        } else {
            c
        }
    };
    if id == 0 && content.is_empty() {
        return None;
    }
    let votes = value
        .get("total_vote")
        .or_else(|| value.get("votes"))
        .and_then(json_i64)
        .or_else(|| {
            value
                .get("voters")
                .and_then(serde_json::Value::as_array)
                .map(|a| a.len() as i64)
        })
        .unwrap_or(0);
    let voted = value
        .get("voted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Some(PollChoice {
        id,
        content,
        votes,
        voted,
    })
}

/// `~/.cache/zalo-rust/thumbs/<fnv(path)>-<edge>.png`, `None` in tests.
fn thumb_path(src: &Path, edge: u32) -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let dir = media_root()?.join("thumbs");
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = std::fs::metadata(src).ok().map(|m| m.len()).unwrap_or(0);
    Some(dir.join(format!(
        "{}-{stamp}-{edge}.png",
        url_key(&src.display().to_string())
    )))
}

/// JPEG/PNG/GIF/WebP via `image`, else JPEG XL via `zjxl`; downscale only.
#[must_use]
pub fn decode_rgba(bytes: &[u8], max_edge: u32) -> Option<image::RgbaImage> {
    if bytes.is_empty() || max_edge == 0 {
        return None;
    }
    if let Ok(img) = image::load_from_memory(bytes) {
        let img = if img.width().max(img.height()) > max_edge {
            img.resize(max_edge, max_edge, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let rgba = img.into_rgba8();
        return (rgba.width() > 0 && rgba.height() > 0).then_some(rgba);
    }
    decode_jxl(bytes, max_edge)
}

fn decode_jxl(bytes: &[u8], max_edge: u32) -> Option<image::RgbaImage> {
    let info = zjxl::dimensions(bytes).ok()?;
    // Same guard as `zalo-gui`: refuse absurd headers before rendering.
    zimage::validate_dimensions(info.width, info.height).ok()?;
    let mut px = zjxl::decode(bytes).ok()?;
    if px.width.max(px.height) > max_edge {
        let edge = i32::try_from(max_edge).ok()?;
        let (w, h) = if px.width >= px.height {
            (edge, -1)
        } else {
            (-1, edge)
        };
        px = zjxl::resize(px, w, h).ok()?;
    }
    let channels = px.channels as usize;
    if !(1..=4).contains(&channels) {
        return None;
    }
    let rgba: Vec<u8> = px
        .data
        .chunks_exact(channels)
        .flat_map(|p| match channels {
            1 => [p[0], p[0], p[0], 255],
            2 => [p[0], p[0], p[0], p[1]],
            3 => [p[0], p[1], p[2], 255],
            _ => [p[0], p[1], p[2], p[3]],
        })
        .collect();
    image::RgbaImage::from_raw(px.width, px.height, rgba)
}

/// GPUI paints `RenderImage` as BGRA (see `gpui::elements::img`).
#[must_use]
pub fn rgba_to_bgra(mut img: image::RgbaImage) -> image::RgbaImage {
    for pixel in img.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    img
}

/// Locate the bundled `assets/images` directory across installed runtime dirs or the dev tree.
#[must_use]
pub(crate) fn images_dir() -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(custom) = std::env::var("ZALO_IMAGES_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed));
        }
    }
    if let Some(data) = zalo_shell::data_home() {
        candidates.push(data.join("zalo-rust").join("images"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("images"));
            candidates.push(parent.join("assets").join("images"));
            candidates.push(parent.join("../share/zalo-rust/images"));
            #[cfg(target_os = "macos")]
            candidates.push(parent.join("../Resources/zalo-rust/images"));
        }
    }
    candidates.push(PathBuf::from("/app/share/zalo-rust/images"));
    candidates.push(PathBuf::from("/usr/share/zalo-rust/images"));
    candidates.push(PathBuf::from("/usr/local/share/zalo-rust/images"));
    let dev = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../assets/images"
    ));
    candidates.push(dev.clone());
    candidates
        .into_iter()
        .find(|dir| dir.is_dir())
        .unwrap_or(dev)
}

/// PNG from `assets/images/<file>`, decoded once (BGRA).
#[must_use]
pub fn png_asset(file: &str, max_edge: u32) -> Option<Arc<RenderImage>> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, Option<Arc<RenderImage>>>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!("{file}@{max_edge}");
    if let Some(hit) = cache.lock().ok().and_then(|m| m.get(&key).cloned()) {
        return hit;
    }
    let path = images_dir().join(file);
    let image = std::fs::read(path)
        .ok()
        .and_then(|b| decode_rgba(&b, max_edge))
        .map(|rgba| Arc::new(RenderImage::new([Frame::new(rgba_to_bgra(rgba))])));
    if let Ok(mut m) = cache.lock() {
        m.insert(key, image.clone());
    }
    image
}

/// Zalo's yellow `Like_24_emoji` (embedded PNG so packaged builds always render it).
/// GPUI cannot paint COLRv1 Noto emoji directly.
#[must_use]
pub fn like_icon() -> Option<Arc<RenderImage>> {
    reaction_icon_by_index(0).or_else(|| {
        const BYTES: &[u8] = include_bytes!("../../../../assets/images/like-24-emoji@4x.png");
        static LIKE: std::sync::OnceLock<Option<Arc<RenderImage>>> = std::sync::OnceLock::new();
        LIKE.get_or_init(|| {
            let rgba = decode_rgba(BYTES, 96)?;
            Some(Arc::new(RenderImage::new([Frame::new(rgba_to_bgra(rgba))])))
        })
        .clone()
    })
}

/// Official Zalo 6-emoji reaction set (`Like`, `Heart`, `Haha`, `Wow`, `Cry`, `Angry`),
/// rasterised from Zalo PC's `*_24_emoji.svg`, embedded in the binary, and
/// cached once in GPU-ready BGRA [`RenderImage`] form.
#[must_use]
pub fn reaction_icon_by_index(index: usize) -> Option<Arc<RenderImage>> {
    const BYTES: [&[u8]; 6] = [
        include_bytes!("../../../../assets/images/reactions/like.png"),
        include_bytes!("../../../../assets/images/reactions/heart.png"),
        include_bytes!("../../../../assets/images/reactions/haha.png"),
        include_bytes!("../../../../assets/images/reactions/wow.png"),
        include_bytes!("../../../../assets/images/reactions/cry.png"),
        include_bytes!("../../../../assets/images/reactions/angry.png"),
    ];
    static ICONS: std::sync::OnceLock<[Option<Arc<RenderImage>>; 6]> = std::sync::OnceLock::new();
    let table = ICONS.get_or_init(|| {
        std::array::from_fn(|i| {
            let rgba = decode_rgba(BYTES[i], 96)?;
            Some(Arc::new(RenderImage::new([Frame::new(rgba_to_bgra(rgba))])))
        })
    });
    table.get(index).cloned().flatten()
}

/// File-type chip label (`PDF`, `ZIP`, …).
#[must_use]
pub fn file_ext_label(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_uppercase())
        .filter(|e| e.len() <= 4)
        .unwrap_or_else(|| str_vi("STR_FILE", "File").to_owned())
}

/// Electron `tfe.ext_list`: text files that get the CodeMirror preview.
const TEXT_PREVIEW_EXTS: &[&str] = &[
    "txt", "html", "css", "c", "cc", "cpp", "cs", "js", "jsx", "ts", "tsx", "json", "log", "java",
    "py", "readme", "sql", "xml", "md",
];

/// First 1024 bytes, same cap as the file-thumb text loader (`{end:1024}`).
const TEXT_PREVIEW_BYTES: usize = 1024;

/// Tone of one run inside a text-file preview line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewTone {
    /// Body text, `--text-primary`.
    Body,
    /// Markdown heading (`cm-header`, light `#00f`).
    Header,
    /// Inline code or a comment (`cm-comment` `#a50`).
    Code,
}

/// Extension Electron previews, or `None` for binaries.
#[must_use]
pub fn text_file_ext(name: &str) -> Option<&'static str> {
    let path = Path::new(name);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let key = if ext.is_empty() { name } else { ext };
    let lower = key.to_ascii_lowercase();
    TEXT_PREVIEW_EXTS.iter().copied().find(|ext| *ext == lower)
}

/// `sizeToString(n, 2)`: two decimals, or a whole number when it divides evenly.
#[must_use]
pub fn format_file_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value.fract() == 0.0 {
        format!("{} {}", value as u64, UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Color runs for one preview line. Markdown headings and `` `code` `` match
/// the CodeMirror colours Electron uses on the file thumb.
#[must_use]
pub fn preview_runs(line: &str, ext: &str) -> Vec<(String, PreviewTone)> {
    let markdown = ext.eq_ignore_ascii_case("md");
    let trimmed = line.trim_start();
    if !markdown
        && (trimmed.starts_with("//")
            || ((ext == "py" || ext == "sh" || ext == "rb") && trimmed.starts_with('#')))
    {
        return vec![(line.to_owned(), PreviewTone::Code)];
    }
    let heading = markdown && trimmed.starts_with('#');
    split_code_spans(
        line,
        if heading {
            PreviewTone::Header
        } else {
            PreviewTone::Body
        },
    )
}

fn split_code_spans(line: &str, body: PreviewTone) -> Vec<(String, PreviewTone)> {
    if !line.contains('`') {
        return vec![(line.to_owned(), body)];
    }
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        if start > 0 {
            out.push((rest[..start].to_owned(), body));
        }
        let after = &rest[start + 1..];
        if let Some(end) = after.find('`') {
            out.push((format!("`{}`", &after[..end]), PreviewTone::Code));
            rest = &after[end + 1..];
        } else {
            out.push((rest[start..].to_owned(), body));
            return out;
        }
    }
    if !rest.is_empty() {
        out.push((rest.to_owned(), body));
    }
    out
}

/// First 1024 bytes of a local text file, split into lines. Cached by path
/// and mtime so painting the bubble does not re-read the disk.
#[must_use]
pub fn read_text_preview(path: &Path) -> Option<Vec<String>> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::SystemTime;
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let key = (path.to_string_lossy().into_owned(), modified, meta.len());
    static CACHE: std::sync::OnceLock<Mutex<HashMap<(String, SystemTime, u64), Vec<String>>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|m| m.get(&key).cloned()) {
        return Some(hit);
    }
    let bytes = std::fs::read(path).ok()?;
    let take = bytes.len().min(TEXT_PREVIEW_BYTES);
    let text = utf8_prefix(&bytes[..take]);
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let lines = if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    };
    if let Ok(mut map) = cache.lock() {
        map.insert(key, lines.clone());
    }
    Some(lines)
}

/// Longest prefix of `buf` that is valid UTF-8.
fn utf8_prefix(buf: &[u8]) -> String {
    let mut end = buf.len();
    while end > 0 && std::str::from_utf8(&buf[..end]).is_err() && buf.len() - end < 4 {
        end -= 1;
    }
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Badge color for a file extension.
#[must_use]
pub fn file_ext_color(name: &str) -> u32 {
    match file_ext_label(name).as_str() {
        "PDF" => 0xdb_36_42,
        "DOC" | "DOCX" => 0x00_5a_e0,
        "XLS" | "XLSX" => 0x15_a8_5f,
        "PPT" | "PPTX" => 0xe8_8c_28,
        "ZIP" | "RAR" | "7Z" | "DEB" | "APK" => 0x78_5a_c8,
        "MD" => 0x00_5a_e0,
        "PNG" | "JPG" | "JPEG" | "GIF" | "WEBP" | "BMP" | "HEIC" => 0x15_a8_5f,
        _ => 0x5a_69_81,
    }
}

/// Map link for a location bubble. Keeps an http(s) `href`; otherwise builds
/// a Google Maps query from coordinates. `(0, 0)` with no href is empty.
#[must_use]
pub fn maps_href(href: &str, lat: f64, lng: f64) -> String {
    if href.starts_with("https://") || href.starts_with("http://") {
        return href.to_owned();
    }
    if lat.is_finite() && lng.is_finite() && (lat.abs() > 0.000_001 || lng.abs() > 0.000_001) {
        return format!("https://www.google.com/maps?q={lat},{lng}");
    }
    String::new()
}

/// A message whose first line is only a Vietnamese phone number.
/// Electron draws this as the blue "linked phone number" card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneNote {
    /// Digits shown as the link, `0` + 9 digits.
    pub phone: String,
    /// Lines under the number, such as a name the sender typed.
    pub caption: String,
}

/// `Some` when `text` starts with a phone and nothing else on that line.
#[must_use]
pub fn phone_note(text: &str) -> Option<PhoneNote> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next()?;
    let phone = vn_phone_line(first)?;
    let caption = lines.collect::<Vec<_>>().join("\n");
    Some(PhoneNote { phone, caption })
}

/// A whole line that is a VN mobile number (`0xxxxxxxxx` or `+84xxxxxxxxx`).
#[must_use]
pub fn vn_phone_line(line: &str) -> Option<String> {
    if line
        .chars()
        .any(|c| !c.is_ascii_digit() && c != '+' && !c.is_whitespace() && c != '-' && c != '.')
    {
        return None;
    }
    let digits: String = line.chars().filter(|c| c.is_ascii_digit()).collect();
    let local = if digits.len() == 11 && digits.starts_with("84") {
        format!("0{}", &digits[2..])
    } else if digits.len() == 10 && digits.starts_with('0') {
        digits
    } else {
        return None;
    };
    // Mobile prefixes Zalo treats as a linked number, not a random id.
    let prefix = local.get(1..3)?;
    let ok = matches!(
        prefix,
        "32" | "33"
            | "34"
            | "35"
            | "36"
            | "37"
            | "38"
            | "39"
            | "52"
            | "55"
            | "56"
            | "58"
            | "59"
            | "70"
            | "76"
            | "77"
            | "78"
            | "79"
            | "81"
            | "82"
            | "83"
            | "84"
            | "85"
            | "86"
            | "87"
            | "88"
            | "89"
            | "90"
            | "91"
            | "92"
            | "93"
            | "94"
            | "96"
            | "97"
            | "98"
            | "99"
    );
    ok.then_some(local)
}

/// Call, file, link, voice and the other rich cards keep the clock on the
/// chat canvas. Text stays inside the bubble. Photos, stickers, GIFs and
/// doodles already use the pill under the picture.
#[must_use]
pub fn clock_below_card(kind: MsgKind) -> bool {
    matches!(
        kind,
        MsgKind::Call
            | MsgKind::MissedCall
            | MsgKind::File
            | MsgKind::Link
            | MsgKind::Location
            | MsgKind::Ecard
            | MsgKind::Voice
            | MsgKind::Video
            | MsgKind::Poll
            | MsgKind::Card
    )
}

/// True when `text` is a raw mobile/local file URI (e.g. Android
/// `content://media/external/images/media/17201`) that Zalo sometimes puts in
/// photo `title` instead of a user caption.
#[must_use]
pub fn is_local_media_uri(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.starts_with("content://")
        || lower.starts_with("file://")
        || lower.starts_with("/storage/")
        || lower.starts_with("/data/")
        || lower.starts_with("/sdcard/")
}

/// True when the body is only a media marker (do not also paint it as text).
#[must_use]
pub fn hide_marker_text(kind: MsgKind, text: &str) -> bool {
    let t = text.trim();
    match kind {
        MsgKind::Photo => {
            t == "[Hình ảnh]" || t == "[chat.photo]" || t.is_empty() || is_local_media_uri(t)
        }
        MsgKind::Sticker => t == "[Sticker]" || t == "[chat.sticker]" || t.is_empty(),
        MsgKind::Gif => {
            t == "[GIF]" || t == "[chat.gif]" || t.is_empty() || is_local_media_uri(t)
        }
        MsgKind::File => t.starts_with("[File]"),
        MsgKind::Voice => t == "[Tin nhắn thoại]" || t == "[Voice]" || t == "[chat.voice]",
        MsgKind::Video => t == "[Video]" || t == "[chat.video.msg]" || is_local_media_uri(t),
        MsgKind::Poll => true,
        MsgKind::Doodle => {
            t == "[Vẽ hình]"
                || t == "[Hình vẽ]"
                || t == "[chat.doodle]"
                || t.is_empty()
                || is_local_media_uri(t)
        }
        MsgKind::Location | MsgKind::Ecard => true,
        MsgKind::Call | MsgKind::MissedCall | MsgKind::Card | MsgKind::Link => true,
        MsgKind::Text => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_long_token() {
        let long = "abcdefghij".repeat(12);
        let wrapped = wrap_bubble_text(&long, 200.0);
        assert!(wrapped.contains('\n'), "{wrapped}");
        assert!(text_box_w(&wrapped) <= BUBBLE_INNER + 0.5);
    }

    #[test]
    fn wrap_keeps_short_line() {
        assert_eq!(wrap_bubble_text("Chào", 300.0), "Chào");
        assert!(text_box_w("Chào") < 80.0);
    }

    #[test]
    fn ui_note_keeps_vietnamese_wider_than_a_bubble() {
        let text =
            "Tạo, chỉnh sửa và quản lý phím tắt cho những tin nhắn thường sử dụng trong hội thoại";
        let bubble = wrap_bubble_text(text, 280.0);
        let note = wrap_ui_note(text, 280.0);
        let bubble_first = bubble.lines().next().unwrap_or("").chars().count();
        let note_first = note.lines().next().unwrap_or("").chars().count();
        assert!(
            note_first > bubble_first,
            "note {note_first} bubble {bubble_first}"
        );
        assert_eq!(wrap_ui_note("Không ảnh nền", 200.0), "Không ảnh nền");
    }

    #[test]
    fn vietnamese_bubble_does_not_wrap_prematurely() {
        let login_msg = "Đăng nhập Zalo trên máy tính thành công";
        let wrapped = wrap_bubble_text(login_msg, BUBBLE_INNER);
        assert_eq!(
            wrapped, login_msg,
            "Vietnamese text fitting in BUBBLE_INNER must stay on one line"
        );
        let guard_msg = "Cùng Zalo bảo vệ tài khoản của bạn";
        assert_eq!(wrap_bubble_text(guard_msg, BUBBLE_INNER), guard_msg);
        assert!(cache_path_for("http://cdn.example/a.jpg").is_some());
        assert!(cache_path_for("https://cdn.example/a.jpg").is_some());

        // Sync 2 server quote + poll metadata.
        let synced_quote = r#"{"msgType":"webchat","content":"ok","quote":{"fromD":"Trường","msg":"hello","globalMsgId":"42","cliMsgId":"99"}}"#;
        let qm = classify("ok", Some(synced_quote));
        assert_eq!(qm.quote_sender, "Trường");
        assert_eq!(qm.quote_text, "hello");
        assert_eq!(qm.quote_global_id, Some(42));
        assert_eq!(qm.quote_cli_id, Some(99));
    }

    #[test]
    fn classify_markers() {
        assert_eq!(classify("[Hình ảnh]", None).kind, MsgKind::Photo);
        assert_eq!(classify("[Sticker]", None).kind, MsgKind::Sticker);
        assert_eq!(classify("[GIF]", None).kind, MsgKind::Gif);
        assert_eq!(classify("[Cuộc gọi nhỡ]", None).kind, MsgKind::MissedCall);
        assert_eq!(classify("[Cuộc gọi]", None).kind, MsgKind::Call);
        let file = classify("[File] notes.md", None);
        assert_eq!(file.kind, MsgKind::File);
        assert_eq!(file.title, "notes.md");
        let link = classify("https://www.youtube.com/watch?v=1", None);
        assert_eq!(link.kind, MsgKind::Link);
        assert_eq!(link.title, "www.youtube.com");
        assert_eq!(classify("xin chào", None).kind, MsgKind::Text);
        let poll = classify("[Bình chọn] Ăn gì?", None);
        assert_eq!(poll.kind, MsgKind::Poll);
        assert_eq!(poll.title, "Ăn gì?");
        let place = classify("[Vị trí] Quán cà phê", None);
        assert_eq!(place.kind, MsgKind::Location);
        assert_eq!(place.title, "Quán cà phê");
        assert_eq!(classify("[Vẽ hình]", None).kind, MsgKind::Doodle);
        assert_eq!(classify("[Hình vẽ]", None).kind, MsgKind::Doodle);
        let card = classify("[Thiệp] Chúc mừng", None);
        assert_eq!(card.kind, MsgKind::Ecard);
        assert_eq!(card.title, "Chúc mừng");
        let located = classify(
            "[Vị trí] gần đây",
            Some(
                r#"{"kind":"location","title":"Hồ Gươm","href":"https://maps.example/x","thumb":"https://t.example/a.jpg"}"#,
            ),
        );
        assert_eq!(located.kind, MsgKind::Location);
        assert_eq!(located.title, "Hồ Gươm");
        assert_eq!(located.href, "https://maps.example/x");
        assert_eq!(
            maps_href("", 21.0285, 105.8542),
            "https://www.google.com/maps?q=21.0285,105.8542"
        );
        assert_eq!(
            maps_href("https://maps.example/x", 1.0, 2.0),
            "https://maps.example/x"
        );
        assert!(maps_href("", 0.0, 0.0).is_empty());
    }

    #[test]
    fn classify_poll_attachment_and_detail() {
        let json = r#"{"kind":"poll","poll_id":9,"question":"Ăn gì?","action":"create","options":[{"id":3,"content":"Phở","votes":2,"voted":true}]}"#;
        let meta = classify("[Bình chọn] Ăn gì?", Some(json));
        assert_eq!(meta.kind, MsgKind::Poll);
        assert_eq!(meta.poll_id, 9);
        assert_eq!(meta.title, "Ăn gì?");
        assert_eq!(meta.poll_options.len(), 1);
        assert!(meta.poll_options[0].voted);
        let detail = serde_json::json!({"data": {"poll_id": 9, "question": "Ăn gì?", "status": 1, "options": [
            {"option_id": 3, "content": "Phở", "voters": [1, 2], "voted": false}
        ]}});
        let body = poll_from_value(&detail).unwrap();
        assert!(body.closed);
        assert_eq!(body.options[0].votes, 2);
        let msg = serde_json::json!({"action": "share", "params": "{\"pollId\":42,\"question\":\"Đi đâu?\"}"});
        let from_msg = poll_from_message(&msg);
        assert_eq!(
            (
                from_msg.id,
                from_msg.question.as_str(),
                from_msg.action.as_str()
            ),
            (42, "Đi đâu?", "share")
        );
    }

    #[test]
    fn animated_gif_keeps_two_frames() {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Rgba, RgbaImage};
        let frame = |color: [u8; 4]| {
            Frame::from_parts(
                RgbaImage::from_pixel(2, 2, Rgba(color)),
                0,
                0,
                Delay::from_numer_denom_ms(40, 1),
            )
        };
        let mut bytes = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut bytes);
            enc.encode_frame(frame([255, 0, 0, 255])).unwrap();
            enc.encode_frame(frame([0, 255, 0, 255])).unwrap();
        }
        let image = decode_animated(&bytes, 32).expect("gif");
        assert_eq!(image.frame_count(), 2);
        let (num, den) = image.delay(0).numer_denom_ms();
        assert!(num / den.max(1) >= 20);
    }

    #[test]
    fn classify_photo_json_without_network() {
        let json = r#"{"kind":"photo","thumb":"","hd":"","width":640,"height":360,"local":null}"#;
        let meta = classify("[Hình ảnh]", Some(json));
        assert_eq!(meta.kind, MsgKind::Photo);
        assert_eq!(meta.width, 640);
        assert_eq!(meta.height, 360);
        assert!(meta.path.is_none());
        let (w, h) = photo_size(640, 360);
        assert!(w <= PHOTO_MAX_W);
        assert!((w / h - 640.0 / 360.0).abs() < 0.05);
    }

    #[test]
    fn url_key_is_stable() {
        assert_eq!(url_key("a"), url_key("a"));
        assert_ne!(url_key("a"), url_key("b"));
        assert_eq!(url_key("a").len(), 16);
        assert_eq!(file_ext_label("a.apk"), "APK");
        assert_eq!(file_ext_color("a.zip"), 0x78_5a_c8);
        assert!(hide_marker_text(MsgKind::Photo, "[Hình ảnh]"));
        assert!(!hide_marker_text(MsgKind::Text, "hi"));
    }

    #[test]
    fn photo_url_prefers_hd_over_thumb_and_group_call_is_call() {
        let json = r#"{"kind":"photo","thumb":"https://t.example/thumb/a.jpg","hd":"https://b.example/full/a.jpg","width":10,"height":10,"local":null}"#;
        assert_eq!(
            classify("[Hình ảnh]", Some(json)).url,
            "https://b.example/full/a.jpg"
        );
        let json = r#"{"kind":"gif","thumb":"","hd":"https://g.example/a.gif","width":10,"height":10,"local":null}"#;
        assert_eq!(classify("[GIF]", Some(json)).url, "https://g.example/a.gif");
        let still =
            r#"{"kind":"gif","thumb":"https://g.example/a.png","hd":"https://g.example/a.gif"}"#;
        assert_eq!(
            classify("[GIF]", Some(still)).url,
            "https://g.example/a.gif"
        );
        let card = r#"{"kind":"card","title":"Lan","thumb":"https://ava.example/a.jpg"}"#;
        let card = classify("[Danh thiếp] Lan", Some(card));
        assert_eq!(card.kind, MsgKind::Card);
        assert_eq!(card.title, "Lan");
        assert_eq!(card.url, "https://ava.example/a.jpg");
        let named = classify("[Danh thiếp] Lan\nBạn cùng lớp", None);
        assert_eq!(named.kind, MsgKind::Card);
        assert_eq!(named.title, "Lan");
        let call = classify("sendGroupCallMessage", None);
        assert_eq!(call.kind, MsgKind::Call);
        assert_eq!(call.title, "Cuộc gọi nhóm");
        assert!(hide_marker_text(call.kind, "sendGroupCallMessage"));
        assert_eq!(classify("Cuộc gọi đi", None).kind, MsgKind::Call);
        assert_eq!(classify("Cuộc gọi nhỡ", None).kind, MsgKind::MissedCall);
        // When `content.href` mirrors `content.thumb`, Sync 2 must pick `params.hd`
        // (full-res uncropped) instead of the blurry cropped `thumb`.
        let synced_photo = r#"{"msgType":"chat.photo","content":{"thumb":"https://t.example/thumb/a.jpg","href":"https://t.example/thumb/a.jpg","params":"{\"width\":1544,\"height\":910,\"hd\":\"https://photo.example/a.jxl\"}"}}"#;
        let photo = classify("[Hình ảnh]", Some(synced_photo));
        assert_eq!(photo.kind, MsgKind::Photo);
        assert_eq!(photo.url, "https://photo.example/a.jxl");
        assert_eq!(photo.width, 1544);
        assert!(hide_marker_text(
            MsgKind::Photo,
            "content://media/external/images/media/17201"
        ));
        let wrapped_file = wrap_text_at(
            "Google_AI_Ultra_Pro_Super_Long_Package_Archive.zip",
            270.0,
            14.0,
            true,
        );
        assert!(
            !wrapped_file.ends_with("\np"),
            "extension must not orphan last char: {wrapped_file}"
        );
        let synced_call = r#"{"msgType":"chat.recommended","content":{"action":"recommened.calltime","title":"sendBubbleMessage","params":"{\"isCaller\":1,\"duration\":12}"}}"#;
        assert_eq!(
            classify("Cuộc gọi đến", Some(synced_call)).kind,
            MsgKind::Call
        );
    }

    #[test]
    fn sender_label_hides_raw_ids() {
        let zid = "MVHFH975IGFTVPPHN3NRUNPFJDPA3C80";
        assert_eq!(sender_label(zid, zid), "Người dùng Zalo");
        assert_eq!(sender_label("1000000000000000003", ""), "Người dùng Zalo");
        assert_eq!(sender_label("1", "Alice Nguyen"), "Alice Nguyen");
        assert_eq!(sender_label("1", "Mai"), "Mai");
    }

    #[test]
    fn classify_extracts_quote_from_text_and_photo_attachments() {
        let q_text =
            r#"{"kind":"text","quote":{"sender":"Lan","text":"Chào nhé","gid":77,"cid":66}}"#;
        let meta = classify("Ok bạn", Some(q_text));
        assert_eq!(meta.kind, MsgKind::Text);
        assert_eq!(meta.quote_sender, "Lan");
        assert_eq!(meta.quote_text, "Chào nhé");
        assert_eq!(
            (meta.quote_global_id, meta.quote_cli_id),
            (Some(77), Some(66))
        );

        let q_photo = r#"{"kind":"photo","thumb":"https://t.example/1.jpg","quote":{"sender":"Tôi","text":"Cái này nè","gid":88}}"#;
        let meta2 = classify("[Hình ảnh]", Some(q_photo));
        assert_eq!(meta2.kind, MsgKind::Photo);
        assert_eq!(meta2.quote_sender, "Tôi");
        assert_eq!(meta2.quote_text, "Cái này nè");
        assert_eq!(
            (meta2.quote_global_id, meta2.quote_cli_id),
            (Some(88), None)
        );
    }

    #[test]
    fn decode_swaps_to_bgra_and_reads_jxl() {
        let mut png = Vec::new();
        let src = image::RgbaImage::from_pixel(4, 2, image::Rgba([255, 0, 0, 255]));
        src.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let rgba = decode_rgba(&png, 100).unwrap();
        assert_eq!(rgba.dimensions(), (4, 2), "no upscale");
        assert_eq!(rgba_to_bgra(rgba).get_pixel(0, 0).0, [0, 0, 255, 255]);

        let red = vec![200_u8, 10, 20, 255].repeat(64 * 32);
        let jxl = zjxl::encode_jxl(&red, 64, 32).expect("encode jxl");
        let out = decode_rgba(&jxl, 16).expect("decode jxl");
        assert_eq!(out.dimensions(), (16, 8));
        let p = out.get_pixel(8, 4).0;
        assert!(p[0] > 150 && p[2] < 80, "{p:?}");
        assert!(decode_rgba(b"not an image", 16).is_none());
    }

    /// Opt-in: `cargo test -p zalo-gpui -- --ignored decode_every_cached`.
    /// Decodes every file in the real `~/.cache/zalo-rust/photos` (no network).
    #[test]
    #[ignore]
    fn decode_every_cached_photo() {
        let Some(dir) = photo_cache_dir() else {
            return;
        };
        let (mut ok, mut bad) = (0, Vec::new());
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            match decode_thumb(&entry.path(), PHOTO_MAX_W as u32) {
                Some(_) => ok += 1,
                None => bad.push(entry.file_name()),
            }
        }
        eprintln!("decoded {ok}, failed {}: {bad:?}", bad.len());
        assert!(bad.is_empty());
    }

    #[test]
    fn bubble_inner_matches_pad() {
        assert_eq!(BUBBLE_INNER, 336.0);
        assert_eq!(BUBBLE_MAX, 360.0);
        assert_eq!(STICKER_SIZE, 120.0);
    }

    #[test]
    fn call_and_file_clocks_leave_the_card() {
        assert!(clock_below_card(MsgKind::Call));
        assert!(clock_below_card(MsgKind::MissedCall));
        assert!(clock_below_card(MsgKind::File));
        assert!(clock_below_card(MsgKind::Link));
        assert!(clock_below_card(MsgKind::Location));
        assert!(clock_below_card(MsgKind::Ecard));
        assert!(clock_below_card(MsgKind::Voice));
        assert!(clock_below_card(MsgKind::Video));
        assert!(clock_below_card(MsgKind::Poll));
        assert!(clock_below_card(MsgKind::Card));
        assert!(!clock_below_card(MsgKind::Text));
        assert!(!clock_below_card(MsgKind::Photo));
        assert!(!clock_below_card(MsgKind::Doodle));
        assert!(!clock_below_card(MsgKind::Sticker));
        assert!(!clock_below_card(MsgKind::Gif));
    }

    #[test]
    fn phone_note_is_the_linked_number_card() {
        let note = phone_note("0901801340\nChị Thúy").expect("phone");
        assert_eq!(note.phone, "0901801340");
        assert_eq!(note.caption, "Chị Thúy");
        assert_eq!(vn_phone_line("+84 901 801 340"), Some("0901801340".into()));
        assert!(phone_note("Gọi cho 0901801340 nhé").is_none());
        assert!(vn_phone_line("1234567890").is_none());
    }

    #[test]
    fn file_size_matches_electron_two_decimals() {
        // 20756 bytes is the 20.27 KB Electron shows on the report card.
        assert_eq!(format_file_size(20_756), "20.27 KB");
        assert_eq!(format_file_size(1024), "1 KB");
        assert_eq!(format_file_size(500), "500 B");
    }

    #[test]
    fn markdown_preview_colours_heading_and_code() {
        assert_eq!(text_file_ext("REPORT.md"), Some("md"));
        assert!(text_file_ext("notes.pdf").is_none());
        assert_eq!(text_file_ext("README"), Some("readme"));
        let heading = preview_runs("# Report", "md");
        assert_eq!(heading, vec![("# Report".into(), PreviewTone::Header)]);
        let code = preview_runs("folder `assets/zcall`", "md");
        assert_eq!(code[0].1, PreviewTone::Body);
        assert_eq!(code[1], ("`assets/zcall`".into(), PreviewTone::Code));
        let dir = std::env::temp_dir().join("zalo-gpui-preview-test.md");
        std::fs::write(&dir, "# Title\n\n`code`\n").unwrap();
        let lines = read_text_preview(&dir).expect("preview");
        assert_eq!(lines[0], "# Title");
        assert!(lines.iter().any(|l| l.contains("code")));
        let _ = std::fs::remove_file(dir);
    }

    #[test]
    fn embedded_like_and_reaction_icons_decode() {
        assert!(like_icon().is_some(), "embedded like icon must decode");
        for idx in 0..6 {
            assert!(
                reaction_icon_by_index(idx).is_some(),
                "embedded reaction icon {idx} must decode"
            );
        }
        assert!(png_asset("zcloud-promo.png", 672).is_some());
    }
}
