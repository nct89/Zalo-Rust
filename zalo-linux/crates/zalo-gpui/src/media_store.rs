//! Conversation media store (Zalo PC `getMediaStorePreview`).
//!
//! The info column asks once per open thread. Group and 1-1 use
//! `/api/mediastore/preview`. "My Cloud" uses `/api/media/oneone/preview`.
//! A failure keeps the photos already loaded in the chat. The PC skips groups
//! when `media_settings.media_cloud` is off (bundle default 0, server config
//! can turn it on). This client tries the group call anyway and falls back.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Context, RenderImage};

use crate::data::ChatMsg;
use crate::media::MsgKind;
use crate::shell::ShellView;
use zalo_client::{MediaItem, MEDIA_PREVIEW_LIMIT};

/// How long a preview (including a failure) is kept.
const SHELF_TTL_MS: i64 = 180_000;

/// Preview cached for the info column.
#[derive(Default)]
pub(crate) struct Shelf {
    pub thread: String,
    pub images: Vec<MediaItem>,
    pub files: Vec<MediaItem>,
    pub links: Vec<MediaItem>,
    pub images_has_more: bool,
    pub files_has_more: bool,
    pub links_has_more: bool,
    pub images_last_id: Option<String>,
    pub files_last_id: Option<String>,
    pub links_last_id: Option<String>,
    pub loading_more: bool,
    pub deleting_ids: HashSet<String>,
    at: i64,
    failed: bool,
    inflight: bool,
    pub thumbs: HashMap<String, Arc<RenderImage>>,
    thumb_pending: HashSet<String>,
}

impl ShellView {
    fn shelf_ready(&self, thread: &str) -> bool {
        self.media_shelf.thread == thread && !self.media_shelf.failed
    }

    /// Up to eight cloud photos/videos for the open info column (Electron `#group-media`).
    pub(crate) fn cloud_photos(&self, thread: &str) -> Vec<MediaItem> {
        if !self.shelf_ready(thread) {
            return Vec::new();
        }
        self.media_shelf.images.iter().take(8).cloned().collect()
    }

    /// Up to three cloud files.
    pub(crate) fn cloud_files(&self, thread: &str) -> Vec<MediaItem> {
        if !self.shelf_ready(thread) {
            return Vec::new();
        }
        self.media_shelf.files.iter().take(3).cloned().collect()
    }

    /// Up to three cloud links.
    pub(crate) fn cloud_links(&self, thread: &str) -> Vec<MediaItem> {
        if !self.shelf_ready(thread) {
            return Vec::new();
        }
        self.media_shelf.links.iter().take(3).cloned().collect()
    }

    /// Every cached photo, for "Xem tất cả" inside the info column.
    pub(crate) fn cloud_images_all(&self, thread: &str) -> Vec<MediaItem> {
        if !self.shelf_ready(thread) {
            return Vec::new();
        }
        self.media_shelf.images.clone()
    }

    /// Every cached file.
    pub(crate) fn cloud_files_all(&self, thread: &str) -> Vec<MediaItem> {
        if !self.shelf_ready(thread) {
            return Vec::new();
        }
        self.media_shelf.files.clone()
    }

    /// Every cached link.
    pub(crate) fn cloud_links_all(&self, thread: &str) -> Vec<MediaItem> {
        if !self.shelf_ready(thread) {
            return Vec::new();
        }
        self.media_shelf.links.clone()
    }

    pub(crate) fn store_thumb(&self, id: &str) -> Option<Arc<RenderImage>> {
        self.media_shelf.thumbs.get(id).cloned()
    }

    /// One preview fetch while the info column is open. Never runs in tests.
    pub(crate) fn drain_media_store(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) || !self.info_open || !self.has_session || self.media_shelf.inflight {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let Some(conv) = self.selected_conv() else {
            return;
        };
        if !conv.live || conv.thread_id.is_empty() {
            return;
        }
        let now = crate::data::now_ms();
        if self.media_shelf.thread == conv.thread_id
            && now.saturating_sub(self.media_shelf.at) < SHELF_TTL_MS
        {
            return;
        }
        let thread = conv.thread_id.clone();
        let send2me = self.send2me_id.as_deref() == Some(thread.as_str());
        self.media_shelf.inflight = true;
        let dir_thumbs = dir.clone();
        cx.spawn(async move |this, cx| {
            let thread_req = thread.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.media_store_preview(&thread_req, send2me, MEDIA_PREVIEW_LIMIT)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.media_shelf.inflight = false;
                let current = view.selected_conv().map(|row| row.thread_id);
                if current.as_deref() != Some(thread.as_str()) {
                    cx.notify();
                    return;
                }
                view.media_shelf.thread = thread;
                view.media_shelf.at = crate::data::now_ms();
                match res {
                    Ok(preview) => {
                        view.media_shelf.images_has_more = !preview.images.is_empty();
                        view.media_shelf.files_has_more = !preview.files.is_empty();
                        view.media_shelf.links_has_more = !preview.links.is_empty();
                        view.media_shelf.images_last_id = preview
                            .images
                            .last()
                            .map(|x| x.id.clone())
                            .filter(|s| !s.is_empty());
                        view.media_shelf.files_last_id = preview
                            .files
                            .last()
                            .map(|x| x.id.clone())
                            .filter(|s| !s.is_empty());
                        view.media_shelf.links_last_id = preview
                            .links
                            .last()
                            .map(|x| x.id.clone())
                            .filter(|s| !s.is_empty());
                        view.media_shelf.images = preview.images;
                        view.media_shelf.files = preview.files;
                        view.media_shelf.links = preview.links;
                        view.media_shelf.failed = false;
                        view.queue_store_thumbs(dir_thumbs, cx);
                    }
                    Err(_) => {
                        view.media_shelf.failed = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn queue_store_thumbs(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let jobs: Vec<(String, String, Option<String>)> = self
            .media_shelf
            .images
            .iter()
            .filter_map(|item| {
                let primary = if item.thumb.starts_with("http") {
                    item.thumb.clone()
                } else if item.url.starts_with("http") {
                    item.url.clone()
                } else {
                    return None;
                };
                let fallback = if item.url.starts_with("http") && item.url != primary {
                    Some(item.url.clone())
                } else {
                    None
                };
                if self.media_shelf.thumbs.contains_key(&item.id)
                    || self.media_shelf.thumb_pending.contains(&item.id)
                    || item.id.is_empty()
                {
                    return None;
                }
                Some((item.id.clone(), primary, fallback))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        for (id, _, _) in &jobs {
            self.media_shelf.thumb_pending.insert(id.clone());
        }
        cx.spawn(async move |this, cx| {
            let fetch_jobs: Vec<crate::fetch::Job> = jobs
                .iter()
                .enumerate()
                .map(|(i, (_, url, _))| crate::fetch::Job {
                    msg_id: i as u64,
                    path: None,
                    url: url.clone(),
                    max_edge: 200,
                })
                .collect();
            let fallbacks: Vec<(u64, String)> = jobs
                .iter()
                .enumerate()
                .filter_map(|(i, (_, _, fb))| fb.as_ref().map(|u| (i as u64, u.clone())))
                .collect();
            let done = cx
                .background_executor()
                .spawn(async move {
                    let mut results = crate::fetch::run_batch(Some(&dir), fetch_jobs);
                    let retry_jobs: Vec<crate::fetch::Job> = results
                        .iter()
                        .filter(|d| d.image.is_none())
                        .filter_map(|d| {
                            let url = fallbacks
                                .iter()
                                .find(|(id, _)| *id == d.msg_id)
                                .map(|(_, u)| u.clone())?;
                            Some(crate::fetch::Job {
                                msg_id: d.msg_id,
                                path: None,
                                url,
                                max_edge: 200,
                            })
                        })
                        .collect();
                    if !retry_jobs.is_empty() {
                        let retried = crate::fetch::run_batch(Some(&dir), retry_jobs);
                        for r in retried {
                            if r.image.is_some() {
                                if let Some(slot) = results.iter_mut().find(|d| d.msg_id == r.msg_id)
                                {
                                    *slot = r;
                                }
                            }
                        }
                    }
                    results
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let mut images = HashMap::new();
                for item in done {
                    if let Some(image) = item.image {
                        images.insert(item.msg_id, image);
                    }
                }
                for (i, (id, _, _)) in jobs.iter().enumerate() {
                    view.media_shelf.thumb_pending.remove(id);
                    if let Some(image) = images.remove(&(i as u64)) {
                        view.media_shelf.thumbs.insert(id.clone(), image);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open a store row. Photos use the viewer once the file is cached.
    /// Files, links and videos open the URL.
    pub(crate) fn open_store_item(
        &mut self,
        item: &MediaItem,
        conv: u64,
        thread: &str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let url = if item.url.starts_with("http") {
            item.url.clone()
        } else {
            item.thumb.clone()
        };
        if !url.starts_with("http") {
            return;
        }
        if item.kind != "image" || item.sub_type == 2 {
            cx.open_url(&url);
            return;
        }
        let msg = store_photo_msg(item, conv, thread, &url);
        if crate::media::cache_path_for(&url).is_some_and(|path| path.is_file()) {
            self.open_viewer(msg, window, cx);
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            cx.open_url(&url);
            return;
        };
        let page = url.clone();
        cx.spawn(async move |this, cx| {
            let fetched = page.clone();
            let done = cx
                .background_executor()
                .spawn(async move {
                    crate::fetch::run_batch(
                        Some(&dir),
                        vec![crate::fetch::Job {
                            msg_id: 1,
                            path: None,
                            url: fetched,
                            max_edge: 1280,
                        }],
                    )
                })
                .await;
            let ok = done.first().and_then(|item| item.image.as_ref()).is_some();
            let _ = this.update(cx, |view, cx| {
                if ok {
                    view.open_photo(msg, false, cx);
                } else {
                    cx.open_url(&page);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// True when the cloud media shelf for `thread` can request another page for `media_type`.
    #[must_use]
    pub(crate) fn media_store_can_load_more(&self, thread: &str, media_type: &str) -> bool {
        if !self.shelf_ready(thread) || !self.has_session {
            return false;
        }
        match media_type {
            "media" => self.media_shelf.images_has_more,
            "file" => self.media_shelf.files_has_more,
            "link" => self.media_shelf.links_has_more,
            _ => false,
        }
    }

    /// Loads the next page of `media_type` (`"media"`, `"file"`, or `"link"`) via `/api/mediastore/list`.
    pub(crate) fn load_more_media_store(
        &mut self,
        thread: &str,
        media_type: &'static str,
        cx: &mut Context<Self>,
    ) {
        if cfg!(test)
            || !self.has_session
            || self.media_shelf.loading_more
            || self.media_shelf.thread != thread
        {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let last_id = match media_type {
            "media" => self.media_shelf.images_last_id.clone(),
            "file" => self.media_shelf.files_last_id.clone(),
            "link" => self.media_shelf.links_last_id.clone(),
            _ => return,
        };
        let send2me = self.send2me_id.as_deref() == Some(thread);
        let thread_owned = thread.to_owned();
        let dir_thumbs = dir.clone();
        self.media_shelf.loading_more = true;
        cx.spawn(async move |this, cx| {
            let thread_req = thread_owned.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.media_store_list(
                            &thread_req,
                            send2me,
                            media_type,
                            Some(30),
                            last_id.as_deref(),
                            None,
                        )
                        .await
                        .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.media_shelf.loading_more = false;
                if view.media_shelf.thread != thread_owned {
                    cx.notify();
                    return;
                }
                match res {
                    Ok(page) => {
                        let next_last = if !page.last_id.is_empty() {
                            Some(page.last_id.clone())
                        } else {
                            page.items
                                .last()
                                .map(|x| x.id.clone())
                                .filter(|s| !s.is_empty())
                        };
                        let added = append_unique_media(
                            match media_type {
                                "media" => &mut view.media_shelf.images,
                                "file" => &mut view.media_shelf.files,
                                _ => &mut view.media_shelf.links,
                            },
                            page.items,
                        );
                        let has_more = page.has_more && added > 0;
                        match media_type {
                            "media" => {
                                view.media_shelf.images_has_more = has_more;
                                if next_last.is_some() {
                                    view.media_shelf.images_last_id = next_last;
                                }
                                if added > 0 {
                                    view.queue_store_thumbs(dir_thumbs, cx);
                                }
                            }
                            "file" => {
                                view.media_shelf.files_has_more = has_more;
                                if next_last.is_some() {
                                    view.media_shelf.files_last_id = next_last;
                                }
                            }
                            _ => {
                                view.media_shelf.links_has_more = has_more;
                                if next_last.is_some() {
                                    view.media_shelf.links_last_id = next_last;
                                }
                            }
                        }
                        if added == 0 {
                            view.set_note("Đã hiển thị toàn bộ mục lưu trữ");
                        }
                    }
                    Err(err) => {
                        view.set_note(format!("Không tải thêm được: {err}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Removes one item from the cloud media store via `/api/mediastore/remove`.
    pub(crate) fn remove_media_store_item(
        &mut self,
        thread: &str,
        media_type: &'static str,
        media_id: String,
        cx: &mut Context<Self>,
    ) {
        if media_id.is_empty() || self.media_shelf.deleting_ids.contains(&media_id) {
            return;
        }
        if cfg!(test) || !self.has_session {
            remove_local_shelf_item(&mut self.media_shelf, media_type, &media_id);
            self.set_note("Đã xóa mục khỏi kho lưu trữ");
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let send2me = self.send2me_id.as_deref() == Some(thread);
        let thread_owned = thread.to_owned();
        self.media_shelf.deleting_ids.insert(media_id.clone());
        cx.spawn(async move |this, cx| {
            let thread_req = thread_owned.clone();
            let id_req = media_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.media_store_remove(&thread_req, send2me, media_type, &id_req)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.media_shelf.deleting_ids.remove(&media_id);
                match res {
                    Ok(()) => {
                        remove_local_shelf_item(&mut view.media_shelf, media_type, &media_id);
                        view.set_note("Đã xóa khỏi kho lưu trữ");
                    }
                    Err(err) if err.contains("212") => {
                        remove_local_shelf_item(&mut view.media_shelf, media_type, &media_id);
                        view.set_note("Mục này đã được xóa trước đó");
                    }
                    Err(err) => {
                        view.set_note(format!("Xóa thất bại: {err}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn append_unique_media(target: &mut Vec<MediaItem>, incoming: Vec<MediaItem>) -> usize {
    let mut seen: HashSet<String> = target
        .iter()
        .filter(|it| !it.id.is_empty())
        .map(|it| it.id.clone())
        .collect();
    let mut added = 0;
    for item in incoming {
        if !item.id.is_empty() && !seen.insert(item.id.clone()) {
            continue;
        }
        target.push(item);
        added += 1;
    }
    added
}

fn remove_local_shelf_item(shelf: &mut Shelf, media_type: &str, media_id: &str) {
    match media_type {
        "media" => {
            shelf.images.retain(|it| it.id != media_id);
            shelf.thumbs.remove(media_id);
        }
        "file" => shelf.files.retain(|it| it.id != media_id),
        "link" => shelf.links.retain(|it| it.id != media_id),
        _ => {}
    }
}

fn store_photo_msg(item: &MediaItem, conv: u64, thread: &str, url: &str) -> ChatMsg {
    let mut msg = ChatMsg::text_msg(
        0,
        conv,
        thread.to_owned(),
        false,
        String::new(),
        String::new(),
        item.title.clone(),
        String::new(),
        0,
        item.cli_msg_id.parse().ok(),
        item.msg_id.parse().ok(),
    );
    msg.kind = MsgKind::Photo;
    msg.media_url = url.to_owned();
    msg.file_name = item.title.clone();
    msg.media_w = item.width;
    msg.media_h = item.height;
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_item(id: &str, kind: &str) -> MediaItem {
        MediaItem {
            id: id.to_owned(),
            kind: kind.to_owned(),
            sub_type: 1,
            msg_id: String::new(),
            cli_msg_id: String::new(),
            title: format!("item-{id}"),
            url: String::new(),
            thumb: String::new(),
            file_size: 0,
            width: 0,
            height: 0,
            sent_ms: 0,
        }
    }

    #[test]
    fn append_unique_deduplicates_and_remove_drops_target() {
        let mut shelf = Shelf::default();
        let added = append_unique_media(
            &mut shelf.images,
            vec![sample_item("1", "image"), sample_item("2", "image")],
        );
        assert_eq!(added, 2);
        let added2 = append_unique_media(
            &mut shelf.images,
            vec![sample_item("2", "image"), sample_item("3", "image")],
        );
        assert_eq!(added2, 1);
        assert_eq!(shelf.images.len(), 3);

        remove_local_shelf_item(&mut shelf, "media", "2");
        let ids: Vec<&str> = shelf.images.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["1", "3"]);
    }
}
