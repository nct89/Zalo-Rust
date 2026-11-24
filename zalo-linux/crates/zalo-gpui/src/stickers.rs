//! Floating `.sticker-selector` (360×420), inline `@gif`/`@sticker`, and sticker suggestions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, hsla, img, prelude::*, px, rgb, Context, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, RenderImage, StatefulInteractiveElement, Styled,
};
use serde_json::Value;
use zalo_client::{GifRef, StickerPack, StickerRef};

use crate::bridge::LiveAction;
use crate::color::{str_vi, token_rgb};
use crate::data::{self, ChatMsg};
use crate::fonts::{self, FAMILY_SEMIBOLD};
use crate::media::{self, MsgKind};
use crate::shell::ShellView;
use crate::widgets::tip;

/// CSS `.sticker-selector` width.
pub const PANEL_W: f32 = zalo_design::STICKER_SELECTOR_W;
/// Height of the floating popup (egui `PANEL_H`, CSS silent).
pub const PANEL_H: f32 = 420.0;

const RECENT_CAP: usize = 24;
const SUGGEST_CAP: usize = 12;
const THUMB_BATCH: usize = 16;

/// Tabs of the sticker popup, same labels as Zalo PC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StickerTab {
    /// Sticker packs + search + store.
    #[default]
    Sticker,
    /// Unicode emoji grid.
    Emoji,
    /// GIF trending + categories + search.
    Gif,
}

impl StickerTab {
    fn label(self) -> &'static str {
        match self {
            Self::Sticker => "STICKER",
            Self::Emoji => "EMOJI",
            Self::Gif => "GIF",
        }
    }
}

/// Active `@sticker` or `@gif` inline composer command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InlineQuery {
    /// `@sticker <keyword>`
    Sticker(String),
    /// `@gif <keyword>`
    Gif(String),
}

/// Cached sticker/GIF store data, suggestions, inline search, and decoded thumbnails.
#[derive(Default)]
pub(crate) struct StickerState {
    pub packs: Vec<StickerPack>,
    pub packs_loaded: bool,
    pub packs_busy: bool,
    pub packs_err: Option<String>,
    pub installed_cates: Vec<i64>,
    pub personal_version: i64,
    pub personal_busy: bool,
    pub expanded: bool,
    pub store_open: bool,
    /// `None` opens the recent strip when non-empty, else the first pack.
    pub pack: Option<i64>,
    pub pack_items: HashMap<i64, Vec<StickerRef>>,
    pub pack_busy: HashSet<i64>,
    pub recent: Vec<StickerRef>,
    pub sticker_q: String,
    pub search_for: String,
    pub search_items: Vec<StickerRef>,
    pub search_busy: bool,
    pub suggest_for: String,
    pub suggest_items: Vec<StickerRef>,
    pub suggest_busy: bool,
    pub gifs: Vec<GifRef>,
    pub gif_cats: Vec<String>,
    pub gif_cat: String,
    pub gif_for: String,
    pub gif_busy: bool,
    pub gif_loaded: bool,
    pub gif_err: Option<String>,
    pub inline: Option<InlineQuery>,
    pub inline_stickers: Vec<StickerRef>,
    pub inline_gifs: Vec<GifRef>,
    pub inline_busy: bool,
    pub thumbs: HashMap<String, Arc<RenderImage>>,
    pub thumb_pending: HashSet<String>,
    pub thumb_failed: HashSet<String>,
    pub thumb_queue: Vec<(String, u32)>,
    /// Active tab on the EMOJI bottom bar (`-1` = Recent, `0..=8` = [`crate::emoji::GROUPS`]).
    pub emoji_group: i8,
    /// Recently inserted emoji strings (newest first) for the `Recent` emoji tab.
    pub recent_emojis: Vec<String>,
}

/// Curated offline sticker tiles shown in preview mode when not signed in.
const PREVIEW_STICKERS: &[(&str, &str, u32)] = &[
    ("👋", "Xin chào!", 0xe6f0ff),
    ("👌", "OK luôn", 0xe8f8f0),
    ("😂", "Haha", 0xfff5d6),
    ("❤️", "Yêu quá", 0xffe8ec),
    ("🙏", "Cảm ơn", 0xf2ebff),
    ("🎉", "Chúc mừng", 0xfff0e0),
    ("☕", "Cà phê nhé", 0xf4efe8),
    ("🔥", "Quá đỉnh!", 0xffebe5),
    ("🥳", "Tuyệt vời", 0xe6f7ff),
];

/// Curated offline GIF tiles shown in preview mode (`emoji`, `label`, `cat`, `bg`).
const PREVIEW_GIFS: &[(&str, &str, &str, u32)] = &[
    ("🐱", "Mèo vẫy tay", "Cười", 0x263238),
    ("😂", "Cười lăn lộn", "Cười", 0x3b2f1e),
    ("❤️", "Thả tim cực mạnh", "Yêu", 0x4a1d28),
    ("🎉", "Tung hoa ăn mừng", "Chúc mừng", 0x2f2542),
    ("👍", "Chuẩn không cần chỉnh", "OK", 0x1b3a4b),
    ("☕", "Cà phê buổi sáng", "Đời sống", 0x3a2e25),
];

/// Parses `@gif <kw>` or `@sticker <kw>` at the start of the composer draft.
#[must_use]
pub(crate) fn inline_query(draft: &str) -> Option<InlineQuery> {
    let trimmed = draft.trim_start();
    if let Some(rest) = strip_command(trimmed, "@gif") {
        return Some(InlineQuery::Gif(rest.to_owned()));
    }
    if let Some(rest) = strip_command(trimmed, "@sticker") {
        return Some(InlineQuery::Sticker(rest.to_owned()));
    }
    None
}

fn strip_command<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = text.get(prefix.len()..)?;
    if rest.is_empty() {
        return Some("");
    }
    rest.chars()
        .next()
        .filter(|c| c.is_whitespace())
        .map(|_| rest.trim())
}

/// Extracts a keyword for `suggest_stickers` when typing ordinary message text.
#[must_use]
pub(crate) fn suggest_keyword(draft: &str) -> Option<String> {
    let trimmed = draft.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('@')
        || inline_query(trimmed).is_some()
    {
        return None;
    }
    let word = trimmed
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim_matches(|c: char| c.is_ascii_punctuation());
    (word.chars().count() >= 2).then(|| word.to_owned())
}

/// Pulls up to 12 category names from Zalo's `get_gif_categories` JSON reply.
#[must_use]
pub(crate) fn gif_category_names(value: &Value) -> Vec<String> {
    let candidates = value
        .get("data")
        .and_then(|d| {
            d.as_array()
                .or_else(|| d.get("categories").and_then(Value::as_array))
                .or_else(|| d.get("data").and_then(Value::as_array))
        })
        .or_else(|| value.get("categories").and_then(Value::as_array))
        .or_else(|| value.as_array());
    let Some(items) = candidates else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let name = item
            .as_str()
            .or_else(|| {
                ["name", "cateName", "title", "tag", "label"]
                    .iter()
                    .find_map(|k| item.get(k).and_then(Value::as_str))
            })
            .unwrap_or("")
            .trim();
        if !name.is_empty() && !out.iter().any(|existing| existing == name) {
            out.push(name.to_owned());
            if out.len() >= 12 {
                break;
            }
        }
    }
    out
}

/// Moves `sticker` to the front of recent history, capped at 24.
pub(crate) fn push_recent_sticker(recent: &mut Vec<StickerRef>, sticker: StickerRef) {
    recent.retain(|s| !(s.id == sticker.id && s.cate_id == sticker.cate_id));
    recent.insert(0, sticker);
    recent.truncate(RECENT_CAP);
}

fn recent_stickers_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let dir = media::media_root()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("gpui-recent-stickers.json"))
}

/// Best-effort read of cached recently-sent stickers (`Vec::new()` in tests).
#[must_use]
pub(crate) fn load_recent_stickers() -> Vec<StickerRef> {
    let Some(path) = recent_stickers_path() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(arr) = serde_json::from_str::<Vec<Value>>(&raw) else {
        return Vec::new();
    };
    arr.into_iter()
        .filter_map(|v| {
            let id = v.get("id")?.as_i64()?;
            let cate_id = v.get("cateId").and_then(Value::as_i64).unwrap_or(0);
            let kind = v.get("type").and_then(Value::as_i64).unwrap_or(1);
            let url = v
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Some(StickerRef {
                id,
                cate_id,
                kind,
                url,
            })
        })
        .take(RECENT_CAP)
        .collect()
}

fn save_recent_stickers(recent: &[StickerRef]) {
    let Some(path) = recent_stickers_path() else {
        return;
    };
    let rows: Vec<Value> = recent
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "cateId": s.cate_id,
                "type": s.kind,
                "url": s.url,
            })
        })
        .collect();
    if let Ok(json) = serde_json::to_string(&rows) {
        let _ = std::fs::write(path, json);
    }
}

fn gif_preview_url(gif: &GifRef) -> &str {
    if gif.thumb.starts_with("https://") {
        &gif.thumb
    } else {
        &gif.url
    }
}

fn fetch_thumb_sync(
    dir: Option<&std::path::Path>,
    url: &str,
    max_edge: u32,
) -> Option<Arc<RenderImage>> {
    if !url.starts_with("https://") {
        let local = std::path::Path::new(url);
        return local
            .is_file()
            .then(|| media::decode_thumb(local, max_edge))
            .flatten();
    }
    let path = media::cache_path_for(url)?;
    if !path.is_file() {
        let dir = dir?;
        let url_owned = url.to_owned();
        let bytes = crate::bridge::block_api(dir, |api| async move {
            api.fetch_limited(&url_owned, 4 << 20)
                .await
                .map_err(|e| e.to_string())
        })
        .ok()?;
        std::fs::write(&path, bytes).ok()?;
    }
    media::decode_thumb(&path, max_edge)
}

impl ShellView {
    /// Queues missing thumbnail URLs for stickers, GIFs, or pack icons.
    pub(crate) fn queue_sticker_thumb(&mut self, url: &str, max_edge: u32) {
        if cfg!(test) {
            return;
        }
        let local = !url.starts_with("https://") && std::path::Path::new(url).is_file();
        if !local && (!self.has_session || !url.starts_with("https://")) {
            return;
        }
        if self.stickers.thumbs.contains_key(url)
            || self.stickers.thumb_pending.contains(url)
            || self.stickers.thumb_failed.contains(url)
        {
            return;
        }
        self.stickers.thumb_pending.insert(url.to_owned());
        self.stickers.thumb_queue.push((url.to_owned(), max_edge));
    }

    fn queue_sticker_list(&mut self, list: &[StickerRef]) {
        for s in list {
            let url = s.image_url();
            self.queue_sticker_thumb(&url, 84);
        }
    }

    fn queue_gif_list(&mut self, list: &[GifRef]) {
        for g in list {
            let url = gif_preview_url(g).to_owned();
            self.queue_sticker_thumb(&url, 160);
        }
    }

    /// Drains queued sticker/GIF/pack thumbnails in background batches.
    pub(crate) fn drain_sticker_thumbs(&mut self, cx: &mut Context<Self>) {
        if self.stickers.thumb_queue.is_empty() {
            return;
        }
        let dir = self.session_dir.clone();
        while !self.stickers.thumb_queue.is_empty() {
            let n = self.stickers.thumb_queue.len().min(THUMB_BATCH);
            let batch: Vec<(String, u32)> = self.stickers.thumb_queue.drain(..n).collect();
            let dir = dir.clone();
            cx.spawn(async move |this, cx| {
                let decoded = cx
                    .background_executor()
                    .spawn(async move {
                        batch
                            .into_iter()
                            .map(|(url, edge)| {
                                let img = fetch_thumb_sync(dir.as_deref(), &url, edge);
                                (url, img)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                let _ = this.update(cx, |view, cx| {
                    for (url, img) in decoded {
                        view.stickers.thumb_pending.remove(&url);
                        match img {
                            Some(im) => {
                                view.stickers.thumbs.insert(url, im);
                            }
                            None => {
                                view.stickers.thumb_failed.insert(url);
                            }
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Loads packs / current pack / trending GIFs on panel open or tab switch.
    pub(crate) fn ensure_sticker_data(&mut self, cx: &mut Context<Self>) {
        if self.sticker_tab == StickerTab::Emoji {
            let _ = crate::emoji::catalog();
        }
        if cfg!(test) || !self.has_session {
            cx.notify();
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        match self.sticker_tab {
            StickerTab::Emoji => {}
            StickerTab::Sticker => {
                if !self.stickers.packs_loaded && !self.stickers.packs_busy {
                    self.stickers.packs_busy = true;
                    self.stickers.packs_err = None;
                    let d = dir.clone();
                    cx.spawn(async move |this, cx| {
                        let res = cx
                            .background_executor()
                            .spawn(async move {
                                crate::bridge::block_api(&d, |api| async move {
                                    let packs =
                                        api.get_sticker_store().await.map_err(|e| e.to_string())?;
                                    let personal = api
                                        .get_personalized_stickers()
                                        .await
                                        .ok()
                                        .map(|v| personalized_stickers_from(&v));
                                    Ok::<_, String>((packs, personal))
                                })
                            })
                            .await;
                        let _ = this.update(cx, |view, cx| {
                            view.stickers.packs_busy = false;
                            match res {
                                Ok((packs, personal)) => {
                                    view.stickers.packs_loaded = true;
                                    if let Some((cates, ver)) = personal {
                                        view.stickers.installed_cates = cates;
                                        view.stickers.personal_version = ver;
                                    }
                                    for p in &packs {
                                        let icon = p.icon.clone();
                                        view.queue_sticker_thumb(&icon, 48);
                                    }
                                    if view.stickers.pack.is_none()
                                        && view.stickers.recent.is_empty()
                                    {
                                        view.stickers.pack = packs
                                            .iter()
                                            .find(|p| view.stickers.installed_cates.contains(&p.id))
                                            .or_else(|| packs.first())
                                            .map(|p| p.id);
                                    }
                                    view.stickers.packs = packs;
                                    if let Some(pid) = view.stickers.pack {
                                        view.load_sticker_pack(pid, cx);
                                    }
                                }
                                Err(err) => view.stickers.packs_err = Some(err),
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                } else if let Some(pid) = self.stickers.pack {
                    self.load_sticker_pack(pid, cx);
                }
            }
            StickerTab::Gif => {
                if !self.stickers.gif_loaded && !self.stickers.gif_busy {
                    self.load_trending_gifs(dir, cx);
                }
            }
        }
    }

    fn load_sticker_pack(&mut self, cate_id: i64, cx: &mut Context<Self>) {
        if cfg!(test) || !self.has_session || self.stickers.pack_items.contains_key(&cate_id) {
            return;
        }
        if !self.stickers.pack_busy.insert(cate_id) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            self.stickers.pack_busy.remove(&cate_id);
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let raw = api
                            .get_category_stickers(cate_id)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>(StickerRef::list_from(&raw))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.stickers.pack_busy.remove(&cate_id);
                match res {
                    Ok(items) => {
                        view.queue_sticker_list(&items);
                        view.stickers.pack_items.insert(cate_id, items);
                    }
                    Err(err) => view.stickers.packs_err = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_trending_gifs(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if cfg!(test) || !self.has_session || self.stickers.gif_busy {
            return;
        }
        self.stickers.gif_busy = true;
        self.stickers.gif_err = None;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let cats_raw = api.get_gif_categories(0, 12).await.ok();
                        let cats = cats_raw
                            .as_ref()
                            .map(gif_category_names)
                            .unwrap_or_default();
                        let gifs_raw = api
                            .get_gif_trending(0, 30)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>((cats, GifRef::list_from(&gifs_raw)))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.stickers.gif_busy = false;
                match res {
                    Ok((cats, gifs)) => {
                        view.stickers.gif_loaded = true;
                        if !cats.is_empty() {
                            view.stickers.gif_cats = cats;
                        }
                        view.queue_gif_list(&gifs);
                        view.stickers.gifs = gifs;
                    }
                    Err(err) => view.stickers.gif_err = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Handles changes in the Sticker tab search box (debounced).
    pub(crate) fn on_sticker_search(&mut self, text: &str, cx: &mut Context<Self>) {
        self.stickers.sticker_q = text.to_owned();
        let kw = text.trim().to_owned();
        if kw.is_empty() {
            self.stickers.search_for.clear();
            self.stickers.search_items.clear();
            self.stickers.search_busy = false;
            cx.notify();
            return;
        }
        if cfg!(test) || !self.has_session || !self.prefs.bool("stickerSearch", true) {
            cx.notify();
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        self.stickers.search_busy = true;
        cx.spawn(async move |this, cx| {
            gpui::Timer::after(Duration::from_millis(350)).await;
            let still = this
                .update(cx, |view, _| view.stickers.sticker_q.trim() == kw)
                .unwrap_or(false);
            if !still {
                return;
            }
            let q = kw.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let raw = api
                            .search_stickers(&q, 40)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>(StickerRef::list_from(&raw))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.stickers.sticker_q.trim() != kw {
                    return;
                }
                view.stickers.search_busy = false;
                view.stickers.search_for = kw;
                match res {
                    Ok(items) => {
                        view.queue_sticker_list(&items);
                        view.stickers.search_items = items;
                    }
                    Err(err) => view.stickers.packs_err = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Handles changes in the GIF tab search box or category chip selection.
    pub(crate) fn on_gif_search(&mut self, keyword: &str, cx: &mut Context<Self>) {
        let kw = keyword.trim().to_owned();
        self.stickers.gif_for = kw.clone();
        if cfg!(test) || !self.has_session {
            cx.notify();
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if kw.is_empty() {
            self.stickers.gif_loaded = false;
            self.load_trending_gifs(dir, cx);
            return;
        }
        if !self.prefs.bool("gifSearch", true) && self.stickers.gif_cat.is_empty() {
            return;
        }
        self.stickers.gif_busy = true;
        cx.spawn(async move |this, cx| {
            gpui::Timer::after(Duration::from_millis(350)).await;
            let still = this
                .update(cx, |view, _| view.stickers.gif_for == kw)
                .unwrap_or(false);
            if !still {
                return;
            }
            let q = kw.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let raw = api
                            .search_gifs(&q, 0, 30)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>(GifRef::list_from(&raw))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.stickers.gif_for != kw {
                    return;
                }
                view.stickers.gif_busy = false;
                match res {
                    Ok(gifs) => {
                        view.queue_gif_list(&gifs);
                        view.stickers.gifs = gifs;
                    }
                    Err(err) => view.stickers.gif_err = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Handles composer text changes for `@gif` / `@sticker` commands and `suggest_stickers`.
    pub(crate) fn on_composer_media_change(&mut self, text: &str, cx: &mut Context<Self>) {
        let inline = inline_query(text);
        if let Some(cmd) = inline {
            self.stickers.suggest_items.clear();
            self.stickers.suggest_for.clear();
            let enabled = match &cmd {
                InlineQuery::Gif(_) => self.prefs.bool("gifSearch", true),
                InlineQuery::Sticker(_) => self.prefs.bool("stickerSearch", true),
            };
            if !enabled {
                self.stickers.inline = None;
                return;
            }
            let changed = self.stickers.inline.as_ref() != Some(&cmd);
            self.stickers.inline = Some(cmd.clone());
            if !changed || cfg!(test) || !self.has_session {
                return;
            }
            let Some(dir) = self.session_dir.clone() else {
                return;
            };
            self.stickers.inline_busy = true;
            cx.spawn(async move |this, cx| {
                gpui::Timer::after(Duration::from_millis(350)).await;
                let still = this
                    .update(cx, |view, _| view.stickers.inline.as_ref() == Some(&cmd))
                    .unwrap_or(false);
                if !still {
                    return;
                }
                let q = cmd.clone();
                let res = cx
                    .background_executor()
                    .spawn(async move {
                        crate::bridge::block_api(&dir, |api| async move {
                            match q {
                                InlineQuery::Sticker(kw) => {
                                    let raw = if kw.is_empty() {
                                        api.search_stickers("hello", 20).await
                                    } else {
                                        api.search_stickers(&kw, 24).await
                                    }
                                    .map_err(|e| e.to_string())?;
                                    Ok::<_, String>((StickerRef::list_from(&raw), Vec::new()))
                                }
                                InlineQuery::Gif(kw) => {
                                    let raw = if kw.is_empty() {
                                        api.get_gif_trending(0, 20).await
                                    } else {
                                        api.search_gifs(&kw, 0, 20).await
                                    }
                                    .map_err(|e| e.to_string())?;
                                    Ok((Vec::new(), GifRef::list_from(&raw)))
                                }
                            }
                        })
                    })
                    .await;
                let _ = this.update(cx, |view, cx| {
                    if view.stickers.inline.as_ref() != Some(&cmd) {
                        return;
                    }
                    view.stickers.inline_busy = false;
                    if let Ok((stickers, gifs)) = res {
                        view.queue_sticker_list(&stickers);
                        view.queue_gif_list(&gifs);
                        view.stickers.inline_stickers = stickers;
                        view.stickers.inline_gifs = gifs;
                    }
                    cx.notify();
                });
            })
            .detach();
            return;
        }

        if self.stickers.inline.is_some() {
            self.stickers.inline = None;
            self.stickers.inline_stickers.clear();
            self.stickers.inline_gifs.clear();
            self.stickers.inline_busy = false;
        }
        if !self.prefs.bool("stickerSuggest", true) {
            self.stickers.suggest_items.clear();
            self.stickers.suggest_for.clear();
            return;
        }
        let Some(kw) = suggest_keyword(text) else {
            self.stickers.suggest_items.clear();
            self.stickers.suggest_for.clear();
            self.stickers.suggest_busy = false;
            return;
        };
        if kw == self.stickers.suggest_for || cfg!(test) || !self.has_session {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        self.stickers.suggest_busy = true;
        let expected = text.to_owned();
        cx.spawn(async move |this, cx| {
            gpui::Timer::after(Duration::from_millis(450)).await;
            let still = this
                .update(cx, |view, cx| view.composer.read(cx).text() == expected)
                .unwrap_or(false);
            if !still {
                return;
            }
            let query = kw.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let raw = api
                            .suggest_stickers(&query)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>(StickerRef::list_from(&raw))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if suggest_keyword(&view.composer.read(cx).text()).as_deref() != Some(&kw) {
                    return;
                }
                view.stickers.suggest_busy = false;
                view.stickers.suggest_for = kw;
                if let Ok(mut list) = res {
                    list.truncate(SUGGEST_CAP);
                    view.queue_sticker_list(&list);
                    view.stickers.suggest_items = list;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Inserts a local sticker bubble and sends via `LiveAction::SendSticker` when live.
    pub(crate) fn send_sticker_now(&mut self, sticker: StickerRef, cx: &mut Context<Self>) {
        let Some(conv) = self.selected_conv() else {
            self.set_note("Chọn một hội thoại để gửi");
            return;
        };
        let now = data::now_ms();
        let cli = now.max(1) as u64;
        let id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.saturating_add(1);
        let url = sticker.image_url();
        if let Some(img) = self.stickers.thumbs.get(&url).cloned() {
            self.media_bitmaps.insert(id, img);
        }
        let mut msg = ChatMsg::text_msg(
            id,
            conv.id,
            conv.thread_id.clone(),
            true,
            str_vi("STR_YOU", "Bạn").to_owned(),
            self.self_uid().unwrap_or_default().to_owned(),
            "[Sticker]".to_owned(),
            data::vn_hm(now),
            now,
            Some(cli as i64),
            None,
        );
        msg.kind = MsgKind::Sticker;
        msg.media_url = url.clone();
        msg.media_path = media::cache_path_for(&url).filter(|p| p.is_file());
        msg.media_w = 130;
        msg.media_h = 130;
        self.push_message(msg);
        self.warm_media(conv.id);

        push_recent_sticker(&mut self.stickers.recent, sticker.clone());
        save_recent_stickers(&self.stickers.recent);
        self.clear_sticker_ui(cx);

        if conv.live && self.has_session {
            self.maybe_live(LiveAction::SendSticker {
                client_id: cli,
                thread_id: conv.thread_id,
                is_group: conv.is_group,
                sticker,
            });
        } else {
            self.set_note("Hội thoại mẫu / chưa đăng nhập — sticker chỉ hiện trên máy này");
        }
    }

    /// Inserts a local GIF bubble and sends via `LiveAction::SendGif` when live.
    pub(crate) fn send_gif_now(&mut self, gif: GifRef, cx: &mut Context<Self>) {
        let Some(conv) = self.selected_conv() else {
            self.set_note("Chọn một hội thoại để gửi");
            return;
        };
        let now = data::now_ms();
        let cli = now.max(1) as u64;
        let id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.saturating_add(1);
        let thumb_url = gif_preview_url(&gif).to_owned();
        if let Some(img) = self.stickers.thumbs.get(&thumb_url).cloned() {
            self.media_bitmaps.insert(id, img);
        }
        let remote_url = if gif.url.starts_with("https://") {
            gif.url.clone()
        } else {
            thumb_url.clone()
        };
        let mut msg = ChatMsg::text_msg(
            id,
            conv.id,
            conv.thread_id.clone(),
            true,
            str_vi("STR_YOU", "Bạn").to_owned(),
            self.self_uid().unwrap_or_default().to_owned(),
            "[GIF]".to_owned(),
            data::vn_hm(now),
            now,
            Some(cli as i64),
            None,
        );
        msg.kind = MsgKind::Gif;
        msg.media_url = remote_url.clone();
        msg.media_path = media::cache_path_for(&remote_url).filter(|p| p.is_file());
        msg.media_w = gif.width.max(160);
        msg.media_h = gif.height.max(120);
        self.push_message(msg);
        self.warm_media(conv.id);
        self.clear_sticker_ui(cx);

        if conv.live && self.has_session {
            self.maybe_live(LiveAction::SendGif {
                client_id: cli,
                thread_id: conv.thread_id,
                is_group: conv.is_group,
                gif,
            });
        } else {
            self.set_note("Hội thoại mẫu / chưa đăng nhập — GIF chỉ hiện trên máy này");
        }
    }

    fn clear_sticker_ui(&mut self, cx: &mut Context<Self>) {
        self.sticker_open = false;
        self.stickers.suggest_items.clear();
        self.stickers.suggest_for.clear();
        if self.stickers.inline.is_some() {
            self.stickers.inline = None;
            self.stickers.inline_stickers.clear();
            self.stickers.inline_gifs.clear();
            self.composer.update(cx, |input, cx| input.clear(cx));
        }
    }

    /// Floating `.sticker-selector` popup above the composer.
    pub(crate) fn sticker_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let line = crate::color::border_subtle();
        let tab = self.sticker_tab;
        let (pw, ph) = if self.stickers.expanded {
            (460.0, 520.0)
        } else {
            (PANEL_W, PANEL_H)
        };
        div()
            .id("sticker-selector")
            .flex()
            .flex_col()
            .w(px(pw))
            .h(px(ph))
            .rounded(px(zalo_design::CARD_RADIUS))
            .bg(rgb(paper))
            .border_1()
            .border_color(line)
            .shadow_sm()
            .overflow_hidden()
            .occlude()
            .child(self.sticker_tabs(cx))
            .child(match tab {
                StickerTab::Emoji => self.emoji_grid(cx).into_any_element(),
                StickerTab::Sticker => self.sticker_tab_body(cx).into_any_element(),
                StickerTab::Gif => self.gif_tab_body(cx).into_any_element(),
            })
    }

    /// Inline `@gif` / `@sticker` or typing-suggestion strip above the composer.
    pub(crate) fn media_strip(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let line = crate::color::border_subtle();
        let bg = token_rgb("--surface-background-subtle", "#f6f8fb");
        let muted = token_rgb("--text-secondary", "#5a6981");
        if let Some(cmd) = &self.stickers.inline {
            let (title, stickers, gifs) = match cmd {
                InlineQuery::Sticker(kw) => (
                    if kw.is_empty() {
                        "Sticker (@sticker <từ khóa>)".to_owned()
                    } else {
                        format!("Sticker: {kw}")
                    },
                    self.stickers.inline_stickers.as_slice(),
                    &[][..],
                ),
                InlineQuery::Gif(kw) => (
                    if kw.is_empty() {
                        "GIF thịnh hành (@gif <từ khóa>)".to_owned()
                    } else {
                        format!("GIF: {kw}")
                    },
                    &[][..],
                    self.stickers.inline_gifs.as_slice(),
                ),
            };
            return Some(
                div()
                    .id("inline-media-strip")
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .px(px(12.))
                    .py(px(8.))
                    .bg(rgb(bg))
                    .border_t_1()
                    .border_color(line)
                    .child(strip_header(title, cx, |this, cx| {
                        this.stickers.inline = None;
                        this.stickers.inline_stickers.clear();
                        this.stickers.inline_gifs.clear();
                        cx.notify();
                    }))
                    .child(if self.stickers.inline_busy {
                        muted_row("Đang tìm…", muted).into_any_element()
                    } else if !stickers.is_empty() {
                        self.sticker_strip_row("inline-stk", stickers, 54., cx)
                            .into_any_element()
                    } else if !gifs.is_empty() {
                        self.gif_strip_row(gifs, cx).into_any_element()
                    } else if !self.has_session {
                        self.preview_inline_strip(cmd, cx).into_any_element()
                    } else {
                        muted_row("Không có kết quả phù hợp", muted).into_any_element()
                    })
                    .into_any_element(),
            );
        }

        if self.stickers.suggest_items.is_empty() || !self.prefs.bool("stickerSuggest", true) {
            return None;
        }
        Some(
            div()
                .id("sticker-suggest-strip")
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(12.))
                .py(px(6.))
                .bg(rgb(bg))
                .border_t_1()
                .border_color(line)
                .child(strip_header(
                    "Gợi ý Sticker".to_owned(),
                    cx,
                    |this, cx| {
                        this.stickers.suggest_items.clear();
                        cx.notify();
                    },
                ))
                .child(self.sticker_strip_row("sug-stk", &self.stickers.suggest_items, 50., cx))
                .into_any_element(),
        )
    }

    fn sticker_strip_row(
        &self,
        prefix: &'static str,
        items: &[StickerRef],
        size: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id((prefix, 0_u64))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .overflow_x_scroll()
            .children(items.iter().take(SUGGEST_CAP).enumerate().map(|(i, s)| {
                let st = s.clone();
                let url = s.image_url();
                let thumb = self.stickers.thumbs.get(&url).cloned();
                div()
                    .id((prefix, i as u64 + 1))
                    .flex_none()
                    .size(px(size))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#e8ecf2"))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send_sticker_now(st.clone(), cx);
                        cx.notify();
                    }))
                    .child(sticker_thumb_el(thumb, size - 6., "☺"))
            }))
    }

    fn gif_strip_row(&self, items: &[GifRef], cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("inline-gif-row")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .overflow_x_scroll()
            .children(items.iter().take(12).enumerate().map(|(i, g)| {
                let gif = g.clone();
                let url = gif_preview_url(g).to_owned();
                let thumb = self.stickers.thumbs.get(&url).cloned();
                div()
                    .id(("inline-gif", i as u64))
                    .flex_none()
                    .w(px(88.))
                    .h(px(60.))
                    .rounded(px(6.))
                    .overflow_hidden()
                    .bg(rgb(token_rgb("--layer-background-hover", "#e8ecf2")))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send_gif_now(gif.clone(), cx);
                        cx.notify();
                    }))
                    .child(gif_thumb_el(thumb, 88., 60.))
            }))
    }

    fn sticker_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let hover_bg = token_rgb("--layer-background-hover", "#f1f2f4");
        let line = crate::color::border_subtle();
        let current = self.sticker_tab;
        let expanded = self.stickers.expanded;
        div()
            .id("sticker-selector-title")
            .flex()
            .flex_none()
            .flex_row()
            .items_center()
            .h(px(zalo_design::STICKER_TITLE_H))
            .px(px(4.))
            .gap(px(0.))
            .border_b_1()
            .border_color(line)
            .child(
                div().flex().flex_row().flex_1().h_full().children(
                    [StickerTab::Sticker, StickerTab::Emoji, StickerTab::Gif]
                        .into_iter()
                        .map(|tab| {
                            let on = tab == current;
                            div()
                                .id(("sticker-tab", tab as u64))
                                .flex()
                                .flex_1()
                                .items_center()
                                .justify_center()
                                .h_full()
                                .mb(px(-1.))
                                .cursor_pointer()
                                .text_size(px(13.5))
                                .line_height(px(21.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(if on { blue } else { muted }))
                                .hover(|style| style.text_color(rgb(ink)))
                                .when(on, |el| el.border_b_2().border_color(rgb(blue)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.sticker_tab = tab;
                                    this.ensure_sticker_data(cx);
                                    cx.notify();
                                }))
                                .child(tab.label())
                        }),
                ),
            )
            .child(
                div()
                    .id("sticker-expand-btn")
                    .size(px(24.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(14.))
                    .text_color(rgb(muted))
                    .hover(move |s| s.bg(rgb(hover_bg)).text_color(rgb(ink)))
                    .tooltip(move |_, cx| tip(if expanded { "Thu gọn" } else { "Mở rộng" }, cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stickers.expanded = !this.stickers.expanded;
                        cx.notify();
                    }))
                    .child(if expanded { "↙" } else { "⤢" }),
            )
            .child(
                div()
                    .id("sticker-close-btn")
                    .size(px(24.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(15.))
                    .text_color(rgb(muted))
                    .hover(move |s| s.bg(rgb(hover_bg)).text_color(rgb(ink)))
                    .tooltip(|_, cx| tip("Đóng", cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sticker_open = false;
                        cx.notify();
                    }))
                    .child(crate::fonts::icon_el(crate::fonts::ic::CLOSE_LINE, "×", 15.)),
            )
    }

    fn emoji_grid(&self, cx: &mut Context<Self>) -> impl IntoElement {
        const EMOJI_CATS: [(i8, &str, char, &str); 10] = [
            (-1, "Gần đây", '\u{EAF5}', "🕒"),
            (0, "Zalo", '\u{EAF6}', "☺"),
            (1, "Con người", '\u{EAF7}', "☺"),
            (2, "Tự nhiên", '\u{EAF8}', "✿"),
            (3, "Đồ ăn", '\u{EAFA}', "☕"),
            (4, "Hoạt động", '\u{EAFB}', "⚽"),
            (5, "Du lịch", '\u{EAFC}', "✈"),
            (6, "Đồ vật", '\u{EAFD}', "💡"),
            (7, "Biểu tượng", '\u{EAFE}', "#"),
            (8, "Quốc gia", '\u{EAFF}', "⚑"),
        ];
        let group = self.stickers.emoji_group.clamp(-1, 8);
        let items: Vec<&crate::emoji::EmojiItem> = if group < 0 {
            let cat = crate::emoji::catalog();
            self.stickers
                .recent_emojis
                .iter()
                .filter_map(|ins| cat.iter().find(|item| item.insert == *ins))
                .collect()
        } else {
            crate::emoji::group_items(group as u8).collect()
        };
        let heading = if group < 0 {
            crate::i18n::t("Sử dụng gần đây")
        } else {
            crate::i18n::t(crate::emoji::GROUPS[group as usize])
        };
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let selected = token_rgb("--layer-background-selected", "#e5f1ff");
        let line = crate::color::border_subtle();
        div()
            .id("sticker-selector-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .child(
                div()
                    .flex_none()
                    .h(px(30.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child(heading),
            )
            .child(
                div()
                    .id("emoji-grid-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(px(6.))
                    .pb(px(8.))
                    .child(if items.is_empty() {
                        div()
                            .p(px(16.))
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child(if group < 0 {
                                "Chưa có emoji sử dụng gần đây"
                            } else {
                                "Không tìm thấy bộ emoji Zalo trên đĩa"
                            })
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .children(items.into_iter().map(|item| {
                                let insert = item.insert.clone();
                                let tip_s = item.tip.clone();
                                let tile = crate::emoji::tile_image(item);
                                let cell_id = crate::emoji::thumb_key(item);
                                div()
                                    .id(gpui::SharedString::from(cell_id))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(38.))
                                    .rounded(px(6.))
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(rgb(hover)))
                                    .tooltip(move |_, cx| tip(tip_s.clone(), cx))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.composer.update(cx, |input, cx| {
                                            input.insert_text(&insert, window, cx);
                                        });
                                        this.stickers.recent_emojis.retain(|s| s != &insert);
                                        this.stickers.recent_emojis.insert(0, insert.clone());
                                        this.stickers.recent_emojis.truncate(40);
                                        let draft = this.composer.read(cx).text();
                                        this.on_composer_media_change(&draft, cx);
                                        let handle = gpui::Focusable::focus_handle(
                                            this.composer.read(cx),
                                            cx,
                                        );
                                        window.focus(&handle);
                                        cx.notify();
                                    }))
                                    .children(tile.map(|img_src| {
                                        img(img_src)
                                            .size(px(28.))
                                            .object_fit(ObjectFit::Contain)
                                    }))
                            }))
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .id("emoji-cat-bar")
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .h(px(zalo_design::STICKER_MENU_H))
                    .px(px(6.))
                    .gap(px(2.))
                    .border_t_1()
                    .border_color(line)
                    .children(EMOJI_CATS.into_iter().map(|(idx, label, glyph, fallback)| {
                        let on = group == idx;
                        let tip_s = crate::i18n::t(label);
                        div()
                            .id(("emo-group", (idx + 1) as u64))
                            .flex()
                            .flex_1()
                            .h(px(32.))
                            .rounded(px(6.))
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(rgb(if on { blue } else { muted }))
                            .when(on, |d| d.bg(rgb(selected)))
                            .hover(move |st| st.bg(rgb(hover)).text_color(rgb(if on { blue } else { ink })))
                            .tooltip(move |_, cx| tip(tip_s.clone(), cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.stickers.emoji_group = idx;
                                cx.notify();
                            }))
                            .child(fonts::icon_el(glyph, fallback, 18.))
                    })),
            )
    }

    fn sticker_tab_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let search_on = self.prefs.bool("stickerSearch", true);
        let searching = search_on && !self.stickers.sticker_q.trim().is_empty();
        let active_title: Option<String> = if self.stickers.store_open {
            Some("Kho sticker Zalo".to_owned())
        } else if self.has_session && !searching {
            match self.stickers.pack {
                None if !self.stickers.recent.is_empty() || self.stickers.packs_loaded => {
                    Some("Sử dụng gần đây".to_owned())
                }
                Some(pid) => self.stickers.packs.iter().find(|p| p.id == pid).map(|p| {
                    if p.name.is_empty() {
                        format!("Bộ sticker #{pid}")
                    } else {
                        p.name.clone()
                    }
                }),
                None => None,
            }
        } else {
            None
        };
        let ink_title = token_rgb("--text-primary", "#081b3a");
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .when(search_on, |el| {
                el.child(search_box(self.sticker_search.clone()))
            })
            .when_some(active_title, |el, title| {
                el.child(
                    div()
                        .id("sticker-selector-content-name")
                        .flex_none()
                        .w_full()
                        .pl(px(16.))
                        .pr(px(8.))
                        .pt(px(4.))
                        .pb(px(3.))
                        .text_size(px(14.))
                        .line_height(px(21.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(ink_title))
                        .truncate()
                        .child(title),
                )
            })
            .child(if self.stickers.store_open && self.has_session {
                self.sticker_store_list(cx).into_any_element()
            } else if !self.has_session {
                self.preview_sticker_grid(cx).into_any_element()
            } else if searching {
                let items = self.stickers.search_items.as_slice();
                if self.stickers.search_busy && items.is_empty() {
                    empty_tab("Sticker", "Đang tìm sticker…").into_any_element()
                } else if items.is_empty() {
                    empty_tab("Sticker", "Không tìm thấy sticker").into_any_element()
                } else {
                    self.sticker_grid("stk-search", items, cx)
                        .into_any_element()
                }
            } else if let Some(err) = &self.stickers.packs_err {
                empty_tab("Sticker", err.clone()).into_any_element()
            } else if self.stickers.pack.is_none() && !self.stickers.recent.is_empty() {
                self.sticker_grid("stk-recent", &self.stickers.recent, cx)
                    .into_any_element()
            } else if self.stickers.pack.is_none() && self.stickers.packs_loaded {
                empty_tab("Sticker", "Chưa có sticker sử dụng gần đây").into_any_element()
            } else if let Some(pid) = self.stickers.pack {
                if let Some(items) = self.stickers.pack_items.get(&pid) {
                    if items.is_empty() {
                        empty_tab("Sticker", "Bộ sticker trống").into_any_element()
                    } else {
                        self.sticker_grid("stk-pack", items, cx).into_any_element()
                    }
                } else {
                    empty_tab("Sticker", "Đang tải bộ sticker…").into_any_element()
                }
            } else if self.stickers.packs_busy {
                empty_tab("Sticker", "Đang tải kho sticker…").into_any_element()
            } else {
                empty_tab("Sticker", "Chưa có bộ sticker nào").into_any_element()
            })
            .child(self.sticker_pack_bar(cx))
    }

    fn sticker_grid(
        &self,
        prefix: &'static str,
        items: &[StickerRef],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cell = zalo_design::STICKER_ITEM;
        let pad = zalo_design::STICKER_ITEM_PAD;
        let thumb_px = cell - pad * 2.0;
        div()
            .id("sticker-selector-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .children(items.iter().enumerate().map(|(i, s)| {
                        let st = s.clone();
                        let url = s.image_url();
                        let thumb = self.stickers.thumbs.get(&url).cloned();
                        div()
                            .id((prefix, i as u64))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(cell))
                            .p(px(pad))
                            .cursor_pointer()
                            .hover(|style| {
                                style.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4")))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_sticker_now(st.clone(), cx);
                                cx.notify();
                            }))
                            .child(sticker_thumb_el(thumb, thumb_px, "…"))
                    })),
            )
    }

    fn sticker_store_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--blue", "#0068ff");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let card = token_rgb("--layer-background", "#ffffff");
        let busy = self.stickers.personal_busy;
        div()
            .id("sticker-store-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p(px(8.))
            .gap(px(4.))
            .children(self.stickers.packs.iter().enumerate().map(|(i, pack)| {
                let pid = pack.id;
                let active = self.stickers.pack == Some(pid);
                let installed = self.stickers.installed_cates.contains(&pid);
                let thumb = self.stickers.thumbs.get(&pack.icon).cloned();
                div()
                    .id(("store-pack", i as u64))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(active, |el| el.bg(rgb(hover)))
                    .hover(|s| s.bg(rgb(hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.stickers.store_open = false;
                        this.stickers.pack = Some(pid);
                        this.load_sticker_pack(pid, cx);
                        cx.notify();
                    }))
                    .child(sticker_thumb_el(thumb, 28., "☺"))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(13.))
                            .text_color(rgb(ink))
                            .truncate()
                            .child(if pack.name.is_empty() {
                                format!("Bộ sticker #{pid}")
                            } else {
                                pack.name.clone()
                            }),
                    )
                    .child(
                        div()
                            .id(("store-pack-toggle", i as u64))
                            .flex_none()
                            .px(px(9.))
                            .py(px(3.))
                            .rounded(px(12.))
                            .bg(rgb(if installed { card } else { blue }))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .text_size(px(11.5))
                            .text_color(rgb(if installed { muted } else { 0xff_ffff }))
                            .when(!busy, |el| {
                                el.on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_personalized_pack(pid, cx);
                                    },
                                ))
                            })
                            .child(if installed { "Gỡ" } else { "+ Thêm" }),
                    )
            }))
    }

    /// Adds or removes `cate_id` via `/api/personalized/stickers/update`.
    fn toggle_personalized_pack(&mut self, cate_id: i64, cx: &mut Context<Self>) {
        if self.stickers.personal_busy || cfg!(test) || !self.has_session {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let prev = self.stickers.installed_cates.clone();
        let prev_ver = self.stickers.personal_version;
        let (next, added) = toggle_cate_in_list(&prev, cate_id);
        self.stickers.installed_cates = next.clone();
        self.stickers.personal_busy = true;
        if added {
            self.stickers.pack = Some(cate_id);
            self.load_sticker_pack(cate_id, cx);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.update_personalized_stickers(&next, prev_ver)
                            .await
                            .map(|v| personalized_stickers_from(&v))
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.stickers.personal_busy = false;
                match res {
                    Ok((cates, ver)) => {
                        if !cates.is_empty() {
                            view.stickers.installed_cates = cates;
                        }
                        if ver > view.stickers.personal_version {
                            view.stickers.personal_version = ver;
                        }
                        view.set_note(if added {
                            "Đã thêm bộ sticker"
                        } else {
                            "Đã gỡ bộ sticker khỏi thanh nhanh"
                        });
                    }
                    Err(err) => {
                        view.stickers.installed_cates = prev;
                        view.set_note(format!("Không thể cập nhật kho sticker: {err}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn sticker_pack_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let line = crate::color::border_subtle();
        let active_bg = token_rgb("--layer-background-hover", "#e8eef7");
        let blue = token_rgb("--B55", "#0068ff");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let recent_on = !self.stickers.store_open && self.stickers.pack.is_none();
        let icon = zalo_design::STICKER_PACK_ICON;
        let icon_m = zalo_design::STICKER_PACK_ICON_MARGIN;
        div()
            .id("sticker-selector-menu")
            .flex()
            .flex_none()
            .flex_row()
            .items_center()
            .h(px(zalo_design::STICKER_MENU_H))
            .px(px(4.))
            .border_t_1()
            .border_color(line)
            .child(
                div()
                    .id("pack-recent")
                    .flex_none()
                    .size(px(icon))
                    .mx(px(icon_m))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_color(rgb(if recent_on { blue } else { muted }))
                    .when(recent_on, |d| d.bg(rgb(active_bg)))
                    .hover(move |d| d.bg(rgb(active_bg)))
                    .tooltip(|_, cx| tip("Gần đây", cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stickers.store_open = false;
                        this.stickers.pack = None;
                        cx.notify();
                    }))
                    .child(fonts::icon_el('\u{EAF5}', "🕒", 20.)),
            )
            .child(
                div()
                    .id("pack-strip-scroll")
                    .flex()
                    .flex_1()
                    .min_w(px(0.))
                    .flex_row()
                    .items_center()
                    .overflow_x_scroll()
                    .children({
                        let filtered: Vec<&StickerPack> =
                            if self.stickers.installed_cates.is_empty() {
                                self.stickers.packs.iter().collect()
                            } else {
                                let mut list: Vec<&StickerPack> = self
                                    .stickers
                                    .packs
                                    .iter()
                                    .filter(|p| {
                                        self.stickers.installed_cates.contains(&p.id)
                                            || self.stickers.pack == Some(p.id)
                                    })
                                    .collect();
                                if list.is_empty() {
                                    list = self.stickers.packs.iter().collect();
                                }
                                list
                            };
                        filtered.into_iter().enumerate().map(|(i, pack)| {
                            let pid = pack.id;
                            let on = !self.stickers.store_open && self.stickers.pack == Some(pid);
                            let thumb = self.stickers.thumbs.get(&pack.icon).cloned();
                            let name = pack.name.clone();
                            div()
                                .id(("pack-btn", i as u64))
                                .flex_none()
                                .size(px(icon))
                                .mx(px(icon_m))
                                .pt(px(2.))
                                .rounded(px(6.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .when(on, |d| d.bg(rgb(active_bg)))
                                .hover(|d| d.bg(rgb(active_bg)))
                                .tooltip(move |_, cx| {
                                    tip(
                                        if name.is_empty() {
                                            "Sticker".to_owned()
                                        } else {
                                            name.clone()
                                        },
                                        cx,
                                    )
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.stickers.store_open = false;
                                    this.stickers.pack = Some(pid);
                                    this.load_sticker_pack(pid, cx);
                                    cx.notify();
                                }))
                                .child(sticker_thumb_el(thumb, icon, "☺"))
                        })
                    }),
            )
            .child(
                div()
                    .id("pack-store-btn")
                    .flex_none()
                    .size(px(icon))
                    .mx(px(icon_m))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(self.stickers.store_open, |d| d.bg(rgb(active_bg)))
                    .hover(|d| d.bg(rgb(active_bg)))
                    .tooltip(|_, cx| tip("Kho sticker", cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stickers.store_open = !this.stickers.store_open;
                        cx.notify();
                    }))
                    .child("+"),
            )
    }

    fn gif_tab_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let search_on = self.prefs.bool("gifSearch", true);
        let active_bg = token_rgb("--B55", "#0068ff");
        let chip_bg = token_rgb("--layer-background-hover", "#f1f2f4");
        let ink = token_rgb("--text-primary", "#081b3a");
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .when(search_on, |el| {
                el.child(search_box(self.gif_search.clone()))
            })
            .when(!self.stickers.gif_cats.is_empty(), |el| {
                let cur = self.stickers.gif_cat.clone();
                el.child(
                    div()
                        .id("gif-cats")
                        .flex()
                        .flex_none()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .px(px(10.))
                        .pb(px(6.))
                        .overflow_x_scroll()
                        .child(gif_chip(
                            "gif-cat-all",
                            "Thịnh hành",
                            cur.is_empty(),
                            active_bg,
                            chip_bg,
                            ink,
                            cx.listener(|this, _, _, cx| {
                                this.stickers.gif_cat.clear();
                                this.on_gif_search("", cx);
                                cx.notify();
                            }),
                        ))
                        .children(self.stickers.gif_cats.iter().enumerate().map(|(i, cat)| {
                            let label = cat.clone();
                            let on = cur == label;
                            let kw = label.clone();
                            gif_chip(
                                ("gif-cat", i as u64),
                                label,
                                on,
                                active_bg,
                                chip_bg,
                                ink,
                                cx.listener(move |this, _, _, cx| {
                                    this.stickers.gif_cat = kw.clone();
                                    this.on_gif_search(&kw, cx);
                                    cx.notify();
                                }),
                            )
                        })),
                )
            })
            .child(if !self.has_session {
                self.preview_gif_grid(cx).into_any_element()
            } else if let Some(err) = &self.stickers.gif_err {
                empty_tab("GIF", err.clone()).into_any_element()
            } else if self.stickers.gif_busy && self.stickers.gifs.is_empty() {
                empty_tab("GIF", "Đang tải GIF…").into_any_element()
            } else if self.stickers.gifs.is_empty() {
                empty_tab("GIF", "Không tìm thấy GIF phù hợp").into_any_element()
            } else {
                self.gif_grid(cx).into_any_element()
            })
    }

    fn gif_grid(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("sticker-selector-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(10.))
            .pb(px(10.))
            .child(div().flex().flex_row().flex_wrap().gap(px(8.)).children(
                self.stickers.gifs.iter().enumerate().map(|(i, g)| {
                    let gif = g.clone();
                    let url = gif_preview_url(g).to_owned();
                    let thumb = self.stickers.thumbs.get(&url).cloned();
                    div()
                        .id(("gif-tile", i as u64))
                        .w(px(164.))
                        .h(px(104.))
                        .rounded(px(8.))
                        .overflow_hidden()
                        .bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4")))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send_gif_now(gif.clone(), cx);
                            cx.notify();
                        }))
                        .child(gif_thumb_el(thumb, 164., 104.))
                }),
            ))
    }

    fn preview_sticker_grid(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.stickers.sticker_q.trim().to_lowercase();
        let muted = token_rgb("--text-secondary", "#5a6981");
        let ink = token_rgb("--text-primary", "#081b3a");
        let blue = token_rgb("--B55", "#0068ff");
        let items: Vec<(usize, &'static str, &'static str, u32)> = PREVIEW_STICKERS
            .iter()
            .enumerate()
            .filter(|(_, (_, label, _))| q.is_empty() || label.to_lowercase().contains(&q))
            .map(|(i, (em, lbl, bg))| (i, *em, *lbl, *bg))
            .collect();
        div()
            .id("preview-sticker-selector-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(12.))
            .py(px(8.))
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(muted))
                            .child("Bộ sticker xem trước — bấm để gửi thử"),
                    )
                    .child(
                        div()
                            .id("stk-switch-emoji")
                            .text_size(px(12.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(blue))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sticker_tab = StickerTab::Emoji;
                                cx.notify();
                            }))
                            .child("Mở Emoji ›"),
                    ),
            )
            .child(if items.is_empty() {
                div()
                    .py(px(24.))
                    .text_size(px(13.))
                    .text_color(rgb(muted))
                    .child("Không có sticker mẫu phù hợp")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(8.))
                    .children(items.into_iter().map(|(i, em, label, bg)| {
                        let card_bg = if crate::color::is_dark() {
                            token_rgb("--layer-background-hover", "#2d323a")
                        } else {
                            bg
                        };
                        div()
                            .id(("prv-stk", i as u64))
                            .w(px(104.))
                            .h(px(88.))
                            .rounded(px(8.))
                            .bg(rgb(card_bg))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.border_1().border_color(rgb(blue)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_draft(&format!("{em} {label}"), cx);
                                this.sticker_open = false;
                                cx.notify();
                            }))
                            .child(div().text_size(px(28.)).line_height(px(30.)).child(em))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(ink))
                                    .child(label),
                            )
                    }))
                    .into_any_element()
            })
    }

    fn preview_gif_grid(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.stickers.gif_for.trim().to_lowercase();
        let cat = self.stickers.gif_cat.clone();
        let muted = token_rgb("--text-secondary", "#5a6981");
        let chip_bg = token_rgb("--layer-background-hover", "#f1f2f4");
        let active_bg = token_rgb("--B55", "#0068ff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let cats = ["Cười", "Yêu", "OK", "Chúc mừng", "Đời sống"];
        let items: Vec<(usize, &'static str, &'static str, u32)> = PREVIEW_GIFS
            .iter()
            .enumerate()
            .filter(|(_, (_, label, c, _))| {
                (cat.is_empty() || *c == cat) && (q.is_empty() || label.to_lowercase().contains(&q))
            })
            .map(|(i, (em, lbl, _, bg))| (i, *em, *lbl, *bg))
            .collect();
        div()
            .id("preview-gif-selector-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(10.))
            .pb(px(10.))
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(gif_chip(
                        "prv-gif-all",
                        "Tất cả",
                        cat.is_empty(),
                        active_bg,
                        chip_bg,
                        ink,
                        cx.listener(|this, _, _, cx| {
                            this.stickers.gif_cat.clear();
                            cx.notify();
                        }),
                    ))
                    .children(cats.iter().enumerate().map(|(i, c)| {
                        let name = (*c).to_owned();
                        gif_chip(
                            ("prv-gif-cat", i as u64),
                            *c,
                            cat == *c,
                            active_bg,
                            chip_bg,
                            ink,
                            cx.listener(move |this, _, _, cx| {
                                this.stickers.gif_cat = name.clone();
                                cx.notify();
                            }),
                        )
                    })),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(muted))
                    .child("GIF mẫu xem trước — đăng nhập để tải kho GIF Zalo"),
            )
            .child(if items.is_empty() {
                div()
                    .py(px(20.))
                    .text_size(px(13.))
                    .text_color(rgb(muted))
                    .child("Không có GIF mẫu phù hợp")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(8.))
                    .children(items.into_iter().map(|(i, em, label, bg)| {
                        div()
                            .id(("prv-gif", i as u64))
                            .relative()
                            .w(px(164.))
                            .h(px(94.))
                            .rounded(px(8.))
                            .bg(rgb(bg))
                            .px(px(10.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(4.))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_draft(&format!("[GIF] {em} {label}"), cx);
                                this.sticker_open = false;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .absolute()
                                    .top(px(6.))
                                    .left(px(6.))
                                    .px(px(5.))
                                    .rounded(px(3.))
                                    .bg(hsla(0., 0., 1., 0.22))
                                    .text_size(px(10.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(0xffffff))
                                    .child("GIF"),
                            )
                            .child(div().text_size(px(24.)).line_height(px(26.)).child(em))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(0xffffff))
                                    .truncate()
                                    .child(label),
                            )
                    }))
                    .into_any_element()
            })
    }

    fn preview_inline_strip(
        &self,
        query: &InlineQuery,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chip_bg = token_rgb("--layer-background-hover", "#f1f2f4");
        let ink = token_rgb("--text-primary", "#081b3a");
        let kw = match query {
            InlineQuery::Sticker(k) | InlineQuery::Gif(k) => k.trim().to_lowercase(),
        };
        let rows: Vec<(usize, String, String)> = match query {
            InlineQuery::Sticker(_) => PREVIEW_STICKERS
                .iter()
                .enumerate()
                .filter(|(_, (_, l, _))| kw.is_empty() || l.to_lowercase().contains(&kw))
                .map(|(i, (e, l, _))| (i, format!("{e} {l}"), format!("{e} {l}")))
                .collect(),
            InlineQuery::Gif(_) => PREVIEW_GIFS
                .iter()
                .enumerate()
                .filter(|(_, (_, l, _, _))| kw.is_empty() || l.to_lowercase().contains(&kw))
                .map(|(i, (e, l, _, _))| (i, format!("GIF {e} {l}"), format!("[GIF] {e} {l}")))
                .collect(),
        };
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.))
            .children(rows.into_iter().map(|(i, chip_label, body)| {
                div()
                    .id(("prv-inline", i as u64))
                    .px(px(10.))
                    .h(px(28.))
                    .rounded(px(14.))
                    .bg(rgb(chip_bg))
                    .flex()
                    .items_center()
                    .text_size(px(12.5))
                    .text_color(rgb(ink))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send_draft(&body, cx);
                        this.composer.update(cx, |input, cx| input.clear(cx));
                        this.clear_sticker_ui(cx);
                        cx.notify();
                    }))
                    .child(chip_label)
            }))
    }
}

fn search_box(input: gpui::Entity<crate::line_input::LineInput>) -> impl IntoElement {
    let field = token_rgb("--input-field-bg-filled", "#f4f6f9");
    let line = crate::color::border_subtle();
    let ink = token_rgb("--text-primary", "#081b3a");
    let muted = token_rgb("--text-secondary", "#5a6981");
    div().px(px(10.)).py(px(6.)).child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .px(px(10.))
            .py(px(5.))
            .rounded(px(6.))
            .bg(rgb(field))
            .border_1()
            .border_color(line)
            .text_size(px(13.))
            .text_color(rgb(ink))
            .child(
                div()
                    .text_color(rgb(muted))
                    .child(fonts::icon_el('\u{ED15}', "⌕", 14.)),
            )
            .child(div().flex_1().child(input)),
    )
}

fn gif_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    on: bool,
    active_bg: u32,
    chip_bg: u32,
    ink: u32,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .px(px(10.))
        .py(px(3.))
        .rounded(px(12.))
        .cursor_pointer()
        .text_size(px(12.))
        .bg(rgb(if on { active_bg } else { chip_bg }))
        .text_color(rgb(if on { 0xffffff } else { ink }))
        .on_click(on_click)
        .child(label.into())
}

fn sticker_thumb_el(
    thumb: Option<Arc<RenderImage>>,
    size: f32,
    fallback: &'static str,
) -> gpui::AnyElement {
    match thumb {
        Some(image) => img(image)
            .size(px(size))
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
        None => div()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(14.))
            .text_color(rgb(token_rgb("--text-secondary", "#5a6981")))
            .child(fallback)
            .into_any_element(),
    }
}

fn gif_thumb_el(thumb: Option<Arc<RenderImage>>, w: f32, h: f32) -> gpui::AnyElement {
    match thumb {
        Some(image) => img(image)
            .w(px(w))
            .h(px(h))
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        None => div()
            .w(px(w))
            .h(px(h))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(12.))
            .text_color(rgb(token_rgb("--text-secondary", "#5a6981")))
            .child("GIF")
            .into_any_element(),
    }
}

fn strip_header(
    title: String,
    cx: &mut Context<ShellView>,
    on_close: fn(&mut ShellView, &mut Context<ShellView>),
) -> impl IntoElement {
    let muted = token_rgb("--text-secondary", "#5a6981");
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(12.))
                .font_family(FAMILY_SEMIBOLD)
                .text_color(rgb(muted))
                .child(title),
        )
        .child(
            div()
                .id("strip-close")
                .px(px(6.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(13.))
                .text_color(rgb(muted))
                .hover(|s| s.bg(hsla(0., 0., 0., 0.08)))
                .on_click(cx.listener(move |this, _, _, cx| on_close(this, cx)))
                .child("×"),
        )
}

fn muted_row(text: &'static str, color: u32) -> impl IntoElement {
    div().text_size(px(12.)).text_color(rgb(color)).child(text)
}

fn empty_tab(title: &'static str, body: impl Into<gpui::SharedString>) -> impl IntoElement {
    let ink = token_rgb("--text-primary", "#081b3a");
    let muted = token_rgb("--text-secondary", "#5a6981");
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .px(px(24.))
        .gap(px(8.))
        .child(
            div()
                .text_size(px(16.))
                .font_family(FAMILY_SEMIBOLD)
                .text_color(rgb(ink))
                .child(title),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(rgb(muted))
                .child(body.into()),
        )
}

/// Parses `/api/personalized/stickers/get` response (`sticker_cates` / `stickerCates` + `version`).
#[must_use]
pub fn personalized_stickers_from(value: &serde_json::Value) -> (Vec<i64>, i64) {
    let root = value.get("data").unwrap_or(value);
    let cates = root
        .get("sticker_cates")
        .or_else(|| root.get("stickerCates"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.as_i64()
                        .or_else(|| item.as_str().and_then(|s| s.parse::<i64>().ok()))
                })
                .collect()
        })
        .unwrap_or_default();
    let version = root.get("version").and_then(|v| v.as_i64()).unwrap_or(0);
    (cates, version)
}

/// Toggles `cate_id` in `list` and returns `(updated, added)`.
#[must_use]
pub fn toggle_cate_in_list(list: &[i64], cate_id: i64) -> (Vec<i64>, bool) {
    let mut next = list.to_vec();
    if let Some(pos) = next.iter().position(|&id| id == cate_id) {
        next.remove(pos);
        (next, false)
    } else {
        next.push(cate_id);
        (next, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticker_panel_matches_css() {
        assert_eq!(PANEL_W, 360.0);
        assert_eq!(PANEL_H, 420.0);
        assert_eq!(zalo_design::STICKER_MENU_H, 56.0);
        assert_eq!(zalo_design::STICKER_TITLE_H, 30.0);
        assert_eq!(zalo_design::STICKER_ITEM, 65.0);
        assert_eq!(zalo_design::STICKER_ITEM_PAD, 3.5);
        assert_eq!(zalo_design::STICKER_PACK_ICON, 30.0);
        assert_eq!(StickerTab::Sticker.label(), "STICKER");
        assert!(!PREVIEW_STICKERS.is_empty());
        assert!(!PREVIEW_GIFS.is_empty());
    }

    #[test]
    fn inline_command_and_suggest_keyword_parse() {
        assert_eq!(
            inline_query("  @gif cat"),
            Some(InlineQuery::Gif("cat".into()))
        );
        assert_eq!(inline_query("@GIF"), Some(InlineQuery::Gif(String::new())));
        assert_eq!(
            inline_query("@Sticker ok"),
            Some(InlineQuery::Sticker("ok".into()))
        );
        assert_eq!(inline_query("@gifx test"), None);
        assert_eq!(inline_query("hello world"), None);

        assert_eq!(suggest_keyword("chào bạn hi!"), Some("hi".into()));
        assert_eq!(suggest_keyword("@gif cat"), None);
        assert_eq!(suggest_keyword("/help"), None);
        assert_eq!(suggest_keyword("a"), None);
    }

    #[test]
    fn gif_categories_and_recent_stickers_dedupe() {
        let raw = serde_json::json!({
            "data": [
                {"name": "Cười"},
                {"cateName": "Buồn"},
                {"name": "Cười"},
                {"title": "Yêu"}
            ]
        });
        assert_eq!(gif_category_names(&raw), vec!["Cười", "Buồn", "Yêu"]);

        let mut recent = Vec::new();
        for id in 1..=26 {
            push_recent_sticker(
                &mut recent,
                StickerRef {
                    id,
                    cate_id: 10,
                    kind: 1,
                    url: String::new(),
                },
            );
        }
        assert_eq!(recent.len(), RECENT_CAP);
        assert_eq!(recent[0].id, 26);

        push_recent_sticker(
            &mut recent,
            StickerRef {
                id: 5,
                cate_id: 10,
                kind: 1,
                url: String::new(),
            },
        );
        assert_eq!(recent.len(), RECENT_CAP);
        assert_eq!(recent[0].id, 5);
        assert_eq!(recent.iter().filter(|s| s.id == 5).count(), 1);
    }

    #[test]
    fn parses_and_toggles_personalized_stickers() {
        let raw = serde_json::json!({
            "data": { "sticker_cates": [101, "202"], "version": 7 }
        });
        let (cates, ver) = personalized_stickers_from(&raw);
        assert_eq!(cates, vec![101, 202]);
        assert_eq!(ver, 7);

        let (added, is_add) = toggle_cate_in_list(&cates, 303);
        assert!(is_add);
        assert_eq!(added, vec![101, 202, 303]);

        let (removed, is_add2) = toggle_cate_in_list(&added, 202);
        assert!(!is_add2);
        assert_eq!(removed, vec![101, 303]);
    }
}
