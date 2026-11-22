//! Separate photo window, the Electron popup the live app titles `Zalo Photo`.
//!
//! The page matches that window: conversation name centered in the header,
//! the picture on a dark stage, a date-grouped filmstrip on the right,
//! up/down chevrons, and a footer with the sender, time and icon buttons.
//! A video uses the same chrome and plays the file already on disk.
//! Clicking a photo or video in chat opens this window instead of covering
//! the conversation or jumping straight to the system player.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::data::ChatMsg;
use crate::fonts::FAMILY_SEMIBOLD;
use crate::media::MsgKind;
use gpui::{
    div, hsla, img, prelude::*, px, rgb, size, App, Context, CursorStyle, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, RenderImage, ScrollHandle,
    ScrollWheelEvent, StatefulInteractiveElement, Styled, Window, WindowBounds, WindowDecorations,
    WindowOptions,
};

const TITLEBAR_H: f32 = zalo_design::TITLEBAR_H;
const SUBHEADER_H: f32 = 36.0;
const FOOTER_H: f32 = 56.0;
const RAIL_W: f32 = 112.0;
const IC_SHARE: char = '\u{ED29}';
const IC_DOWNLOAD: char = '\u{EAE1}';
const IC_ROTATE_L: char = '\u{ED0E}';
const IC_ZOOM_IN: char = '\u{EE16}';
const IC_ZOOM_OUT: char = '\u{EE18}';
const IC_COPY: char = '\u{EAC5}';
const IC_OCR: char = '\u{ED0F}';
const IC_UP: char = '\u{EA91}';
const IC_DOWN: char = '\u{EA8B}';

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 6.0;
/// Electron `Zalo Photo` dark canvas (`#141414`), chrome bars (`#282828`), filmstrip (`#1e1e1e`).
const STAGE_BG: u32 = 0x14_14_14;
const BAR_BG: u32 = 0x28_28_28;
const RAIL_BG: u32 = 0x1e_1e_1e;

/// OS title of the live Electron photo window (`xdotool getwindowname` → `Zalo Photo`).
#[must_use]
pub(crate) fn photo_window_title(_conv_name: &str) -> String {
    "Zalo Photo".to_owned()
}

/// http(s) URL for a gallery item. `media_url` wins; `href` is the fallback.
#[must_use]
pub(crate) fn http_media(msg: &ChatMsg) -> String {
    for raw in [&msg.media_url, &msg.href] {
        let url = raw.trim();
        if url.starts_with("http://") || url.starts_with("https://") {
            return url.to_owned();
        }
    }
    String::new()
}

/// The filmstrip can show this item: a file is already on disk, or a URL can be fetched.
#[must_use]
pub(crate) fn has_gallery_source(msg: &ChatMsg) -> bool {
    msg.media_path.as_ref().is_some_and(|path| path.is_file()) || !http_media(msg).is_empty()
}

/// Share clicks in this window ask the main shell to open its forward picker.
fn push_share(msg: ChatMsg) {
    if let Ok(mut queue) = SHARE_Q.lock() {
        queue.push(msg);
    }
}

/// Drain share requests. The shell calls this while painting.
pub(crate) fn take_shares() -> Vec<ChatMsg> {
    SHARE_Q
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

static SHARE_Q: Mutex<Vec<ChatMsg>> = Mutex::new(Vec::new());

/// Open the gallery. `current_id` selects the photo that was clicked.
/// `start_ocr` runs text recognition on that photo (context-menu "Scan Text").
#[allow(clippy::too_many_arguments)]
pub(crate) fn open(
    cx: &mut App,
    conv_name: String,
    items: Vec<ChatMsg>,
    current_id: u64,
    sender_avatars: HashMap<u64, Arc<RenderImage>>,
    initial_thumbs: HashMap<u64, Arc<RenderImage>>,
    download_dir: PathBuf,
    session_dir: Option<PathBuf>,
    start_ocr: bool,
    mute_video: bool,
) -> bool {
    if items.is_empty() {
        return false;
    }
    let title = photo_window_title(&conv_name);
    let bounds = crate::bounds_in_work_area(size(px(1140.), px(740.)), cx);
    crate::x11_frame::anticipate_csd_window();
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            is_resizable: true,
            window_decorations: Some(WindowDecorations::Client),
            window_background: gpui::WindowBackgroundAppearance::Transparent,
            window_min_size: Some(size(px(720.), px(480.))),
            app_id: Some("zalo-gpui".into()),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title(&title);
            let view = cx.new(|cx| {
                PhotoView::new(
                    cx,
                    conv_name,
                    items,
                    current_id,
                    sender_avatars,
                    initial_thumbs,
                    download_dir,
                    session_dir,
                    start_ocr,
                    mute_video,
                )
            });
            let focus = view.focus_handle(cx);
            window.focus(&focus);
            window.activate_window();
            let weak = view.downgrade();
            window
                .spawn(cx, async move |cx| {
                    for delay_ms in [50u64, 180] {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(delay_ms))
                            .await;
                        let _ = weak.update_in(cx, |_, window, cx| {
                            window.refresh();
                            cx.notify();
                        });
                    }
                })
                .detach();
            view
        },
    )
    .is_ok()
}

struct PhotoView {
    focus: FocusHandle,
    conv_name: String,
    items: Vec<ChatMsg>,
    index: usize,
    sender_avatars: HashMap<u64, Arc<RenderImage>>,
    rail_open: bool,
    image: Option<Arc<RenderImage>>,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    turns: u8,
    drag: Option<(f32, f32, f32, f32)>,
    dragged: bool,
    ocr_active: bool,
    ocr_busy: bool,
    ocr_result: Option<Result<String, String>>,
    /// Line boxes from the last successful scan, fractions of the image.
    ocr_boxes: Vec<crate::viewer::OcrBox>,
    /// Selected OCR line box indices (Live-Text style directly on the photo).
    ocr_selected: BTreeSet<usize>,
    /// Active drag marquee `(x0, y0, x1, y1)` in image-local pixels.
    ocr_drag: Option<(f32, f32, f32, f32)>,
    /// Whether the user dismissed the Electron-style blue hint pill for this scan.
    ocr_hint_dismissed: bool,
    ocr_installing: bool,
    ocr_install_status: Option<String>,
    note: Option<String>,
    note_gen: u64,
    download_dir: PathBuf,
    session_dir: Option<PathBuf>,
    /// Drops a decode that finished for a photo the user already left.
    generation: u64,
    thumbs: Vec<Option<Arc<RenderImage>>>,
    film_scroll: ScrollHandle,
    /// Last index passed to [`ScrollHandle::scroll_to_item`].
    film_pinned: usize,
    mute_video: bool,
    /// In-frame ffmpeg frames. `None` while the video is stopped.
    video_pump: Option<VideoPump>,
    video_seq: u64,
}

struct VideoPump {
    stop: Arc<AtomicBool>,
    audio: Option<crate::audio_play::AudioHandle>,
    id: u64,
}

impl PhotoView {
    #[allow(clippy::too_many_arguments)]
    fn new(
        cx: &mut Context<Self>,
        conv_name: String,
        items: Vec<ChatMsg>,
        current_id: u64,
        sender_avatars: HashMap<u64, Arc<RenderImage>>,
        initial_thumbs: HashMap<u64, Arc<RenderImage>>,
        download_dir: PathBuf,
        session_dir: Option<PathBuf>,
        start_ocr: bool,
        mute_video: bool,
    ) -> Self {
        let index = items.iter().position(|m| m.id == current_id).unwrap_or(0);
        let thumbs: Vec<Option<Arc<RenderImage>>> = items
            .iter()
            .map(|m| initial_thumbs.get(&m.id).cloned())
            .collect();
        let mut this = Self {
            focus: cx.focus_handle(),
            conv_name,
            items,
            index,
            sender_avatars,
            rail_open: true,
            image: None,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            turns: 0,
            drag: None,
            dragged: false,
            ocr_active: false,
            ocr_busy: false,
            ocr_result: None,
            ocr_boxes: Vec::new(),
            ocr_selected: BTreeSet::new(),
            ocr_drag: None,
            ocr_hint_dismissed: false,
            ocr_installing: false,
            ocr_install_status: None,
            note: None,
            note_gen: 0,
            download_dir,
            session_dir,
            generation: 1,
            thumbs,
            film_scroll: ScrollHandle::new(),
            film_pinned: usize::MAX,
            mute_video,
            video_pump: None,
            video_seq: 0,
        };
        this.load_thumbs(cx);
        this.reload(cx);
        if start_ocr {
            this.start_ocr(cx);
        }
        this
    }

    fn show_note(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.note = Some(text.into());
        self.note_gen = self.note_gen.wrapping_add(1);
        let gen = self.note_gen;
        cx.spawn(async move |this, cx| {
            gpui::Timer::after(std::time::Duration::from_millis(2400)).await;
            let _ = this.update(cx, |view, cx| {
                if view.note_gen == gen {
                    view.note = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn reset_ocr(&mut self) {
        self.ocr_active = false;
        self.ocr_busy = false;
        self.ocr_result = None;
        self.ocr_boxes.clear();
        self.ocr_selected.clear();
        self.ocr_drag = None;
        self.ocr_hint_dismissed = false;
    }

    fn finish_ocr_drag(
        &mut self,
        pos: gpui::Point<gpui::Pixels>,
        modifiers: gpui::Modifiers,
        window: &Window,
        img_left: f32,
        img_top: f32,
        dw: f32,
        dh: f32,
    ) -> bool {
        let Some((sx, sy, _, _)) = self.ocr_drag.take() else {
            return false;
        };
        let (cx_pos, cy_pos) = crate::window_frame::window_to_chrome(window, pos);
        let lx = (cx_pos - img_left).clamp(0.0, dw);
        let ly = (cy_pos - (TITLEBAR_H + SUBHEADER_H + img_top)).clamp(0.0, dh);
        if (lx - sx).abs() + (ly - sy).abs() <= 4.0 {
            if let Some(hit) = crate::viewer::ocr_box_at(&self.ocr_boxes, lx, ly, dw, dh) {
                if modifiers.control || modifiers.shift {
                    if !self.ocr_selected.remove(&hit) {
                        self.ocr_selected.insert(hit);
                    }
                } else if self.ocr_selected.len() == 1 && self.ocr_selected.contains(&hit) {
                    self.ocr_selected.clear();
                } else {
                    self.ocr_selected.clear();
                    self.ocr_selected.insert(hit);
                }
            } else {
                self.ocr_selected.clear();
            }
        }
        true
    }

    fn copy_ocr_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.ocr_boxes.is_empty() {
            return false;
        }
        let text = crate::viewer::ocr_selected_text(&self.ocr_boxes, &self.ocr_selected);
        if text.is_empty() {
            return false;
        }
        crate::clip::copy_text(cx, text);
        let count = if self.ocr_selected.is_empty() {
            self.ocr_boxes.len()
        } else {
            self.ocr_selected.len()
        };
        self.show_note(
            if count > 1 {
                format!("Đã sao chép {count} dòng văn bản")
            } else {
                "Đã sao chép văn bản".to_owned()
            },
            cx,
        );
        true
    }

    fn current(&self) -> &ChatMsg {
        &self.items[self.index.min(self.items.len() - 1)]
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let msg = self.current().clone();
        if msg.kind == MsgKind::Video {
            self.image = self.thumbs.get(self.index).cloned().flatten();
            cx.notify();
            return;
        }
        let turns = self.turns % 4;
        let generation = self.generation;
        let path = msg.media_path.clone().filter(|p| p.is_file());
        let url = http_media(&msg);
        let session = self.session_dir.clone();
        let msg_id = msg.id;
        cx.spawn(async move |this, cx| {
            let (image, fetched_path) = cx
                .background_executor()
                .spawn(async move {
                    if let Some(img) = decode_photo(path.clone(), &url, turns) {
                        return (Some(img), path);
                    }
                    if crate::media::is_http_url(&url) {
                        let done = crate::fetch::run_batch(
                            session.as_deref(),
                            vec![crate::fetch::Job {
                                msg_id,
                                path: None,
                                url: url.clone(),
                                max_edge: 1200,
                            }],
                        );
                        if let Some(downloaded) = done.into_iter().next().and_then(|d| d.path) {
                            let img = decode_photo(Some(downloaded.clone()), &url, turns);
                            return (img, Some(downloaded));
                        }
                    }
                    (None, path)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(p) = fetched_path {
                    if let Some(slot) = view.items.iter_mut().find(|m| m.id == msg_id) {
                        if slot.kind != MsgKind::Video
                            && slot.media_path.as_ref().is_none_or(|c| !c.is_file())
                        {
                            slot.media_path = Some(p);
                        }
                    }
                }
                if view.generation == generation && view.turns % 4 == turns {
                    if let Some(image) = image.as_ref() {
                        if view
                            .thumbs
                            .get(view.index)
                            .is_some_and(|slot| slot.is_none())
                        {
                            if let Some(slot) = view.thumbs.get_mut(view.index) {
                                *slot = Some(Arc::clone(image));
                            }
                        }
                    }
                    view.image = image;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.stop_video();
        let next = self.index as isize + delta;
        if next < 0 || next >= self.items.len() as isize {
            return;
        }
        self.index = next as usize;
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.turns = 0;
        self.image = None;
        self.reset_ocr();
        self.generation = self.generation.wrapping_add(1);
        self.reload(cx);
    }

    fn set_zoom(&mut self, zoom: f32) {
        if self.current().kind == MsgKind::Video {
            return;
        }
        self.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        if (self.zoom - 1.0).abs() < 0.02 {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
        }
    }

    fn rotate(&mut self, steps: i8, cx: &mut Context<Self>) {
        if self.current().kind == MsgKind::Video {
            return;
        }
        self.turns = (i16::from(self.turns) + i16::from(steps)).rem_euclid(4) as u8;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.image = None;
        self.reset_ocr();
        self.generation = self.generation.wrapping_add(1);
        self.reload(cx);
    }

    fn pixels(&self) -> (f32, f32) {
        if let Some(image) = &self.image {
            let s = image.size(0);
            let (w, h) = (s.width.0 as f32, s.height.0 as f32);
            if w >= 1.0 && h >= 1.0 {
                return (w, h);
            }
        }
        let msg = self.current();
        let mut w = msg.media_w.max(1) as f32;
        let mut h = msg.media_h.max(1) as f32;
        if self.turns % 2 == 1 {
            std::mem::swap(&mut w, &mut h);
        }
        (w, h)
    }

    fn fit(&self, stage_w: f32, stage_h: f32) -> f32 {
        let (w, h) = self.pixels();
        let avail_w = (stage_w - 24.0).max(80.0);
        let avail_h = (stage_h - 16.0).max(80.0);
        (avail_w / w).min(avail_h / h).clamp(0.02, 32.0)
    }

    fn start_ocr(&mut self, cx: &mut Context<Self>) {
        if self.ocr_busy {
            return;
        }
        // Electron `OCR_TOGGLE`: toggle the Live-Text overlay instantaneously
        // when this photo was already scanned; otherwise scan on demand.
        if !self.ocr_boxes.is_empty() {
            self.ocr_active = !self.ocr_active;
            if !self.ocr_active {
                self.ocr_selected.clear();
                self.ocr_drag = None;
            }
            return;
        }
        if matches!(self.ocr_result, Some(Err(_))) {
            self.ocr_result = None;
            self.ocr_active = false;
            return;
        }
        let msg = self.current().clone();
        if !matches!(msg.kind, MsgKind::Photo | MsgKind::Doodle | MsgKind::Gif) {
            return;
        }
        self.ocr_active = true;
        self.ocr_busy = true;
        self.ocr_result = None;
        self.ocr_boxes.clear();
        self.ocr_selected.clear();
        self.ocr_drag = None;
        self.ocr_hint_dismissed = false;
        let turns = self.turns % 4;
        let id = msg.id;
        let path = msg.media_path.clone().filter(|p| p.is_file());
        let url = msg.media_url.clone();
        let dir = self.session_dir.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let file =
                        path.or_else(|| crate::media::cache_path_for(&url).filter(|p| p.is_file()));
                    let bytes = match file {
                        Some(p) => std::fs::read(p).map_err(|e| e.to_string())?,
                        None => match dir {
                            Some(d) if crate::media::is_http_url(&url) => {
                                let u = url.clone();
                                crate::bridge::block_api(&d, |api| async move {
                                    api.fetch_limited(&u, 8 << 20)
                                        .await
                                        .map_err(|e| e.to_string())
                                })?
                            }
                            _ => return Err("Ảnh chưa có trên máy để quét chữ.".to_owned()),
                        },
                    };
                    crate::viewer::recognize_viewer_bytes(&bytes, turns)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.current().id == id {
                    view.ocr_busy = false;
                    match res {
                        Ok(scan) => {
                            view.ocr_active = true;
                            view.ocr_boxes = scan.boxes;
                            view.ocr_result = Some(Ok(scan.text));
                        }
                        Err(err) => {
                            view.ocr_active = false;
                            view.ocr_boxes.clear();
                            view.ocr_result = Some(Err(err));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_ocr_install(&mut self, cx: &mut Context<Self>) {
        if self.ocr_installing {
            return;
        }
        self.ocr_installing = true;
        self.ocr_install_status = Some("Đang kết nối Cloudflare R2...".to_owned());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("zalo-photo-ocr-install".into())
            .spawn(move || {
                match crate::installer::download_and_install_ocr_r2(|ev| {
                    let _ = tx.send(Ok::<_, String>(ev));
                }) {
                    Ok(dir) => {
                        let _ = tx.send(Ok(crate::installer::InstallEvent::Done { exe_path: dir }));
                    }
                    Err(err) => {
                        let _ = tx.send(Err(err));
                    }
                }
            })
            .expect("spawn ocr install thread");

        cx.spawn(async move |this, cx| loop {
            gpui::Timer::after(std::time::Duration::from_millis(60)).await;
            let mut finished = false;
            let mut ok_installed = false;
            let mut err_msg = None;
            let mut status_update = None;
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Ok(crate::installer::InstallEvent::OcrDownload {
                        done_bytes,
                        total_bytes,
                        speed_bps,
                        ..
                    }) => {
                        let frac = (done_bytes as f64 / total_bytes.max(1) as f64).clamp(0.0, 1.0);
                        status_update = Some(format!(
                            "Đang tải OCR từ R2 ({:.0}%): {:.2} / {:.2} MB · {:.2} MB/s",
                            frac * 100.0,
                            done_bytes as f64 / 1_048_576.0,
                            total_bytes as f64 / 1_048_576.0,
                            speed_bps as f64 / 1_048_576.0,
                        ));
                    }
                    Ok(crate::installer::InstallEvent::Step { status, .. }) => {
                        status_update = Some(status);
                    }
                    Ok(crate::installer::InstallEvent::Done { .. }) => {
                        ok_installed = true;
                        finished = true;
                    }
                    Ok(_) => {}
                    Err(err) => {
                        err_msg = Some(err);
                        finished = true;
                    }
                }
            }
            let alive = this.update(cx, |view, cx| {
                if let Some(s) = status_update {
                    view.ocr_install_status = Some(s);
                }
                if ok_installed {
                    view.ocr_installing = false;
                    view.ocr_install_status = None;
                    view.ocr_result = None;
                    view.note = Some("Đã cài xong OCR từ R2".to_owned());
                    view.start_ocr(cx);
                } else if let Some(err) = err_msg {
                    view.ocr_installing = false;
                    view.ocr_install_status = None;
                    view.ocr_result = Some(Err(err));
                }
                cx.notify();
            });
            if alive.is_err() || finished {
                break;
            }
        })
        .detach();
    }

    fn share_current(&mut self, cx: &mut Context<Self>) {
        if self.current().kind == MsgKind::Video {
            self.show_note("Chia sẻ video từ khung chat", cx);
            return;
        }
        push_share(self.current().clone());
        self.show_note("Đã mở hộp chuyển tiếp ở cửa sổ chính", cx);
    }

    fn copy_current(&mut self, cx: &mut Context<Self>) {
        let msg = self.current().clone();
        if msg.kind == MsgKind::Video {
            self.show_note("Video không copy được như ảnh", cx);
            return;
        }
        match copy_photo_bytes(&msg) {
            Ok((format, bytes)) => {
                crate::clip::copy_image(cx, format, bytes);
                self.show_note("Đã copy hình ảnh", cx);
            }
            Err(err) => self.show_note(err, cx),
        }
    }

    fn save_current(&mut self, cx: &mut Context<Self>) {
        let msg = self.current().clone();
        match save_photo_file(&msg, &self.download_dir) {
            Ok(dest) => self.show_note(format!("Đã lưu {dest}"), cx),
            Err(err) => self.show_note(err, cx),
        }
    }

    fn load_thumbs(&mut self, cx: &mut Context<Self>) {
        let session = self.session_dir.clone();
        let center = self.index;
        let n = self.items.len();
        // Order pending thumbnails by proximity to the currently opened photo
        // so the visible filmstrip fills in immediately, streaming in small
        // batches of 4 instead of blocking until the whole thread downloads.
        let mut ordered: Vec<(usize, crate::fetch::Job)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(i, _)| self.thumbs.get(*i).is_none_or(Option::is_none))
            .map(|(i, msg)| {
                (
                    i,
                    crate::fetch::Job {
                        msg_id: msg.id,
                        path: msg.media_path.clone().filter(|path| path.is_file()),
                        url: http_media(msg),
                        max_edge: 160,
                    },
                )
            })
            .collect();
        ordered.sort_by_key(|(i, _)| i.abs_diff(center));
        let jobs: Vec<crate::fetch::Job> = ordered.into_iter().map(|(_, job)| job).collect();
        if jobs.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            for chunk in jobs.chunks(crate::fetch::BUDGET) {
                let batch = chunk.to_vec();
                let dir = session.clone();
                let done = cx
                    .background_executor()
                    .spawn(async move { crate::fetch::run_batch(dir.as_deref(), batch) })
                    .await;
                let alive = this.update(cx, |view, cx| {
                    if view.items.len() != n {
                        return;
                    }
                    let mut touched = false;
                    for item in done {
                        let Some(index) = view.items.iter().position(|msg| msg.id == item.msg_id)
                        else {
                            continue;
                        };
                        if let Some(path) = item.path.clone() {
                            // A video thumb is a still, not the mp4. Keep the real file path.
                            let video = view.items[index].kind == MsgKind::Video;
                            let missing = view.items[index]
                                .media_path
                                .as_ref()
                                .is_none_or(|current| !current.is_file());
                            if missing && !video {
                                view.items[index].media_path = Some(path);
                            }
                        }
                        if item.image.is_some() {
                            view.thumbs[index] = item.image;
                            touched = true;
                        }
                    }
                    let stage_missing = view.image.is_none()
                        && view.current().kind != MsgKind::Video
                        && view
                            .current()
                            .media_path
                            .as_ref()
                            .is_some_and(|path| path.is_file());
                    if view.current().kind == MsgKind::Video && view.image.is_none() {
                        view.image = view.thumbs.get(view.index).cloned().flatten();
                        touched = true;
                    }
                    if stage_missing {
                        view.reload(cx);
                    }
                    if touched || stage_missing {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Scroll the open item into the filmstrip once its row has a real height.
    ///
    /// The first `scroll_to_item` often runs before layout, and GPUI then
    /// drops the request. Latch only after the row exists so a later frame retries.
    fn pin_film(&mut self) {
        let idx = self.index;
        if self.film_pinned == idx {
            return;
        }
        let ready = self
            .film_scroll
            .bounds_for_item(idx)
            .is_some_and(|bounds| f32::from(bounds.size.height) > 1.0);
        self.film_scroll.scroll_to_item(idx);
        if ready {
            self.film_pinned = idx;
        }
    }

    fn play_video(&mut self, cx: &mut Context<Self>) {
        let msg = self.current().clone();
        if msg.kind != MsgKind::Video {
            return;
        }
        if self.video_pump.is_some() {
            self.stop_video();
            self.image = self.thumbs.get(self.index).cloned().flatten();
            self.note = Some("Đã dừng video".to_owned());
            return;
        }
        let Some(path) = video_file(&msg) else {
            self.note = Some("Video chưa có trên máy".to_owned());
            return;
        };
        if self.start_video(path.clone(), cx) {
            return;
        }
        cx.open_with_system(&path);
        self.note = Some("Đang mở video bằng trình phát hệ thống".to_owned());
    }

    fn stop_video(&mut self) {
        if let Some(pump) = self.video_pump.take() {
            pump.stop.store(true, Ordering::Relaxed);
            if let Some(audio) = pump.audio {
                audio.stop();
            }
        }
    }

    /// Start native in-frame H.264 MP4 decode (+ in-process audio when unmuted).
    /// Returns `false` when the container/codec is not an H.264 MP4.
    fn start_video(&mut self, path: PathBuf, cx: &mut Context<Self>) -> bool {
        if crate::video::open_h264_mp4(&path).is_none() {
            return false;
        }
        self.video_seq = self.video_seq.wrapping_add(1);
        let id = self.video_seq;
        let stop = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(Mutex::new(VecDeque::new()));
        if !spawn_mp4_frames(&path, stop.clone(), frames.clone()) {
            return false;
        }
        let audio = if self.mute_video {
            None
        } else {
            crate::audio_play::play_file(path, None, false, None).ok()
        };
        let muted = audio.is_none();
        self.video_pump = Some(VideoPump {
            stop: stop.clone(),
            audio,
            id,
        });
        self.note = Some(if muted {
            "Đang phát trong khung (tắt tiếng)".to_owned()
        } else {
            "Đang phát trong khung".to_owned()
        });
        cx.spawn(async move |this, cx| loop {
            gpui::Timer::after(std::time::Duration::from_millis(33)).await;
            let queue = frames.clone();
            let stopped = stop.load(Ordering::Relaxed);
            let alive = this
                .update(cx, move |view, cx| {
                    if view.video_pump.as_ref().map(|pump| pump.id) != Some(id) {
                        return false;
                    }
                    if let Some(image) = queue
                        .lock()
                        .ok()
                        .and_then(|mut pending| pending.pop_front())
                    {
                        view.image = Some(image);
                        cx.notify();
                    }
                    let empty = queue
                        .lock()
                        .map(|pending| pending.is_empty())
                        .unwrap_or(true);
                    if stopped && empty {
                        view.video_pump = None;
                        cx.notify();
                        return false;
                    }
                    true
                })
                .unwrap_or(false);
            if !alive {
                break;
            }
        })
        .detach();
        true
    }

    fn stage_size(&self, window: &Window) -> (f32, f32) {
        let vp = window.viewport_size();
        let csd = if crate::window_frame::CsdCorners::from_window(window).any_pad() {
            zalo_design::WINDOW_SHADOW * 2.0
        } else {
            0.0
        };
        let rail = if self.rail_open { RAIL_W } else { 0.0 };
        (
            (f32::from(vp.width) - csd - rail).max(200.0),
            (f32::from(vp.height) - csd - TITLEBAR_H - SUBHEADER_H - FOOTER_H).max(80.0),
        )
    }

    fn on_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match key {
            "escape" => {
                if !self.ocr_selected.is_empty() {
                    self.ocr_selected.clear();
                    self.ocr_drag = None;
                } else if self.ocr_active || matches!(self.ocr_result, Some(Err(_))) {
                    self.ocr_active = false;
                    self.ocr_drag = None;
                    if matches!(self.ocr_result, Some(Err(_))) {
                        self.ocr_result = None;
                    }
                } else {
                    window.remove_window();
                }
                return true;
            }
            "left" | "up" => self.step(-1, cx),
            "right" | "down" => self.step(1, cx),
            "space" => {
                if self.current().kind == MsgKind::Video {
                    self.play_video(cx);
                } else {
                    return false;
                }
            }
            "-" | "minus" => self.set_zoom(self.zoom / 1.25),
            "=" | "+" | "equal" => self.set_zoom(self.zoom * 1.25),
            "0" => self.set_zoom(1.0),
            "1" => {
                let (sw, sh) = self.stage_size(window);
                let fit = self.fit(sw, sh);
                self.set_zoom((1.0 / fit).clamp(ZOOM_MIN, ZOOM_MAX));
            }
            "r" => self.rotate(1, cx),
            _ => return false,
        }
        true
    }
}

impl Focusable for PhotoView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PhotoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (stage_w, stage_h) = self.stage_size(window);
        let fit = self.fit(stage_w, stage_h);
        let zoom = self.zoom;
        let (pw, ph) = self.pixels();
        let (dw, dh) = ((pw * fit * zoom).max(1.0), (ph * fit * zoom).max(1.0));
        let percent = (fit * zoom * 100.0).round().clamp(1.0, 9999.0);
        let zoomed = (zoom - 1.0).abs() > 0.03;
        let idx = self.index;
        let has_prev = idx > 0;
        let has_next = idx + 1 < self.items.len();
        let msg = self.current().clone();
        let is_video = msg.kind == MsgKind::Video;
        let playing = self.video_pump.is_some();
        let image = self.image.clone();
        let (pan_x, pan_y) = if is_video {
            (0.0, 0.0)
        } else {
            clamp_pan(self.pan_x, self.pan_y, dw, dh, stage_w, stage_h)
        };
        if !is_video {
            self.pan_x = pan_x;
            self.pan_y = pan_y;
        }
        let img_left = (stage_w - dw) * 0.5 + pan_x;
        let img_top = (stage_h - dh) * 0.5 + pan_y;
        let ocr_busy = self.ocr_busy;
        let ocr_result = self.ocr_result.clone();
        let ocr_boxes = self.ocr_boxes.clone();
        let ocr_live = self.ocr_active && !ocr_boxes.is_empty() && !is_video;
        let ocr_selected = self.ocr_selected.clone();
        let ocr_drag = self.ocr_drag;
        let ocr_hint_dismissed = self.ocr_hint_dismissed;
        let ocr_installing = self.ocr_installing;
        let ocr_install_status = self.ocr_install_status.clone();
        let can_ocr = matches!(msg.kind, MsgKind::Photo | MsgKind::Doodle | MsgKind::Gif);
        let can_pan = !is_video && !ocr_live && (dw > stage_w + 1.0 || dh > stage_h + 1.0);
        let dragging = self.drag.is_some();
        let rail_open = self.rail_open;
        let caption =
            (!crate::media::hide_marker_text(msg.kind, &msg.text)).then(|| msg.text.clone());
        let note = self.note.clone();
        let header = if !self.conv_name.trim().is_empty() {
            self.conv_name.clone()
        } else if msg.sender.trim().is_empty() {
            "Ảnh".to_owned()
        } else {
            msg.sender.clone()
        };
        let sender = if msg.sender.trim().is_empty() {
            header.clone()
        } else {
            msg.sender.clone()
        };
        let sender_avatar = self.sender_avatars.get(&msg.id).cloned();
        let initial = sender.chars().next().unwrap_or('?').to_string();
        let now = crate::data::now_ms();
        let day_tag = filmstrip_day(msg.at_ms, now);
        let when = if !msg.time.trim().is_empty() && !day_tag.is_empty() && !msg.time.contains(&day_tag) {
            format!("{} {day_tag}", msg.time.trim())
        } else if msg.at_ms > 0 {
            crate::data::day_label(msg.at_ms, now)
        } else {
            msg.time.clone()
        };
        if rail_open {
            self.pin_film();
        }

        let icon_btn = |id: &'static str, code: char, label: &'static str, active: bool| {
            div()
                .id(id)
                .size(px(34.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(if active {
                    rgb(0x4d_a3_ff)
                } else {
                    gpui::rgba(0xff_ff_ff_e0)
                })
                .when(active, |el| {
                    el.bg(hsla(0., 0., 1., 0.16))
                        .border_1()
                        .border_color(gpui::rgba(0x4d_a3_ff_66))
                })
                .hover(|st| st.bg(hsla(0., 0., 1., 0.14)))
                .tooltip(move |_, cx| crate::widgets::tip(label, cx))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(crate::fonts::icon_el(code, "•", 18.))
        };
        let bar_divider = || {
            div()
                .w(px(1.))
                .h(px(18.))
                .mx(px(4.))
                .bg(hsla(0., 0., 1., 0.16))
        };

        let metas: Vec<(u64, i64, bool)> = self
            .items
            .iter()
            .map(|m| (m.id, m.at_ms, m.kind == MsgKind::Video))
            .collect();
        let thumbs = self.thumbs.clone();
        let mut prev_day = String::new();
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        if rail_open {
            for (i, (id, at_ms, video)) in metas.into_iter().enumerate() {
                let day = filmstrip_day(at_ms, now);
                let show_day = !day.is_empty() && day != prev_day;
                if show_day {
                    prev_day = day.clone();
                }
                let thumb = thumbs.get(i).cloned().flatten();
                let selected = i == idx;
                let label = day;
                rows.push(
                    div()
                        .id(("zalo-photo-thumb", id))
                        .w(px(RAIL_W))
                        .flex()
                        .flex_col()
                        .items_center()
                        .px(px(10.))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if this.index != i {
                                let delta = i as isize - this.index as isize;
                                this.step(delta, cx);
                            }
                            cx.notify();
                        }))
                        .when(show_day, |el| {
                            el.child(
                                div()
                                    .w_full()
                                    .pt(px(10.))
                                    .pb(px(6.))
                                    .flex()
                                    .justify_center()
                                    .text_size(px(12.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(hsla(0., 0., 1., 0.78))
                                    .child(label.clone()),
                            )
                        })
                        .child({
                            let mut cell = div()
                                .mb(px(8.))
                                .w(px(88.))
                                .h(px(74.))
                                .rounded(px(6.))
                                .overflow_hidden()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(rgb(0x16_18_1a))
                                .border_2()
                                .border_color(if selected {
                                    rgb(0xffffff).into()
                                } else {
                                    hsla(0., 0., 1., 0.)
                                });
                            cell = match thumb {
                                Some(t) => cell.child(
                                    img(t)
                                        .w(px(88.))
                                        .h(px(74.))
                                        .object_fit(gpui::ObjectFit::Cover),
                                ),
                                None => cell.bg(rgb(0x3a_3d_40)).child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(hsla(0., 0., 1., 0.85))
                                        .child(if video {
                                            "▶".to_owned()
                                        } else {
                                            String::new()
                                        })
                                        .when(!video, |el| {
                                            el.child(crate::fonts::icon_el(
                                                crate::fonts::ic::TB_PHOTO,
                                                "▣",
                                                18.,
                                            ))
                                        }),
                                ),
                            };
                            cell
                        })
                        .into_any_element(),
                );
            }
        }

        let missing_ocr = matches!(
            &ocr_result,
            Some(Err(err)) if err == &crate::viewer::ocr_missing_message()
        );
        let ocr_err = match &ocr_result {
            Some(Err(err)) if !missing_ocr => Some(err.clone()),
            _ => None,
        };
        let selected_count = ocr_selected.len();
        let show_blue_pill = ocr_live && (!ocr_selected.is_empty() || !ocr_hint_dismissed);
        let show_status_pill = ocr_busy || ocr_installing || missing_ocr || ocr_err.is_some();

        let frame = crate::window_frame::CsdCorners::from_window(window);
        crate::window_frame::wrap_csd(
            window,
            div()
                .id("zalo-photo")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
                    let mods = &e.keystroke.modifiers;
                    if mods.control && !mods.alt && !mods.platform && !mods.function {
                        match e.keystroke.key.as_str() {
                            "c" => {
                                if this.ocr_active && !this.ocr_boxes.is_empty() {
                                    this.copy_ocr_selection(cx);
                                } else {
                                    this.copy_current(cx);
                                }
                                cx.stop_propagation();
                                cx.notify();
                                return;
                            }
                            "a" if this.ocr_active && !this.ocr_boxes.is_empty() => {
                                this.ocr_selected = (0..this.ocr_boxes.len()).collect();
                                cx.stop_propagation();
                                cx.notify();
                                return;
                            }
                            _ => return,
                        }
                    }
                    if mods.control || mods.alt || mods.platform || mods.function {
                        return;
                    }
                    if this.on_key(&e.keystroke.key, window, cx) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .flex()
                .flex_col()
                .size_full()
                .bg(rgb(STAGE_BG))
                .when(frame.tl, |el| el.rounded_tl(frame.radius))
                .when(frame.tr, |el| el.rounded_tr(frame.radius))
                .when(frame.bl, |el| el.rounded_bl(frame.radius))
                .when(frame.br, |el| el.rounded_br(frame.radius))
                // 1. Electron-style `Zalo Photo` OS caption bar
                .child(crate::titlebar::photo_titlebar(frame, window))
                // 2. Dark sub-header with centered conversation title
                .child(
                    div()
                        .id("zalo-photo-header")
                        .flex()
                        .flex_none()
                        .h(px(SUBHEADER_H))
                        .items_center()
                        .justify_center()
                        .px(px(24.))
                        .bg(rgb(BAR_BG))
                        .border_b_1()
                        .border_color(hsla(0., 0., 1., 0.06))
                        .text_size(px(14.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(0xffffff))
                        .child(div().truncate().child(header)),
                )
                // 3. Stage + optional collapsible filmstrip
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_h(px(0.))
                        .child(
                            div()
                                .id("zalo-photo-stage")
                                .on_scroll_wheel(cx.listener(
                                    |this, e: &ScrollWheelEvent, _, cx| {
                                        cx.stop_propagation();
                                        let dy = f32::from(e.delta.pixel_delta(px(20.)).y);
                                        if dy == 0.0 {
                                            return;
                                        }
                                        let z = this.zoom;
                                        this.set_zoom(if dy > 0.0 { z * 1.1 } else { z / 1.1 });
                                        cx.notify();
                                    },
                                ))
                                .relative()
                                .flex_1()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .when(can_pan && !dragging, |el| el.cursor(CursorStyle::OpenHand))
                                .when(dragging, |el| el.cursor(CursorStyle::ClosedHand))
                                .when(is_video, |el| el.cursor_pointer())
                                .when(!is_video, |el| {
                                    el.on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            if this.ocr_active && !this.ocr_boxes.is_empty() {
                                                // Clicked dark stage outside image: clear text selection.
                                                if !this.ocr_selected.is_empty() {
                                                    this.ocr_selected.clear();
                                                    cx.notify();
                                                }
                                                return;
                                            }
                                            this.dragged = false;
                                            this.drag = Some((
                                                f32::from(e.position.x),
                                                f32::from(e.position.y),
                                                this.pan_x,
                                                this.pan_y,
                                            ));
                                        }),
                                    )
                                    .on_mouse_move(cx.listener(
                                        move |this, e: &gpui::MouseMoveEvent, window, cx| {
                                            if e.pressed_button != Some(gpui::MouseButton::Left) {
                                                let cleared = this.ocr_drag.take().is_some()
                                                    | this.drag.take().is_some();
                                                if cleared {
                                                    cx.notify();
                                                }
                                                return;
                                            }
                                            if let Some((sx, sy, _, _)) = this.ocr_drag {
                                                let (cx_pos, cy_pos) =
                                                    crate::window_frame::window_to_chrome(
                                                        window,
                                                        e.position,
                                                    );
                                                let lx = (cx_pos - img_left).clamp(0.0, dw);
                                                let ly = (cy_pos
                                                    - (TITLEBAR_H + SUBHEADER_H + img_top))
                                                    .clamp(0.0, dh);
                                                this.ocr_drag = Some((sx, sy, lx, ly));
                                                if (lx - sx).abs() + (ly - sy).abs() > 4.0 {
                                                    this.ocr_selected =
                                                        crate::viewer::ocr_boxes_in_rect(
                                                            &this.ocr_boxes,
                                                            sx,
                                                            sy,
                                                            lx,
                                                            ly,
                                                            dw,
                                                            dh,
                                                        );
                                                }
                                                cx.notify();
                                                return;
                                            }
                                            let Some((ox, oy, px0, py0)) = this.drag else {
                                                return;
                                            };
                                            let dx = f32::from(e.position.x) - ox;
                                            let dy = f32::from(e.position.y) - oy;
                                            if dx.abs() + dy.abs() > 3.0 {
                                                this.dragged = true;
                                            }
                                            let (sw, sh) = this.stage_size(window);
                                            let fit = this.fit(sw, sh);
                                            let (pw, ph) = this.pixels();
                                            let cur_dw = (pw * fit * this.zoom).max(1.0);
                                            let cur_dh = (ph * fit * this.zoom).max(1.0);
                                            let (nx, ny) = clamp_pan(
                                                px0 + dx,
                                                py0 + dy,
                                                cur_dw,
                                                cur_dh,
                                                sw,
                                                sh,
                                            );
                                            this.pan_x = nx;
                                            this.pan_y = ny;
                                            cx.notify();
                                        },
                                    ))
                                    .on_mouse_up(
                                        gpui::MouseButton::Left,
                                        cx.listener(
                                            move |this, e: &gpui::MouseUpEvent, window, cx| {
                                                if this.finish_ocr_drag(
                                                    e.position,
                                                    e.modifiers,
                                                    window,
                                                    img_left,
                                                    img_top,
                                                    dw,
                                                    dh,
                                                ) {
                                                    cx.notify();
                                                }
                                                this.drag = None;
                                            },
                                        ),
                                    )
                                    .on_mouse_up_out(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                                            this.drag = None;
                                            if this.ocr_drag.take().is_some() {
                                                cx.notify();
                                            }
                                        }),
                                    )
                                })
                                .when(is_video, |el| {
                                    el.on_click(cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.play_video(cx);
                                        cx.notify();
                                    }))
                                })
                                .child(if is_video {
                                    div()
                                        .relative()
                                        .size_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(match image {
                                            Some(image) => img(image)
                                                .w(px((stage_w - 24.0).max(80.0)))
                                                .h(px((stage_h - 24.0).max(80.0)))
                                                .object_fit(gpui::ObjectFit::Contain)
                                                .into_any_element(),
                                            None => div()
                                                .text_size(px(14.))
                                                .text_color(hsla(0., 0., 1., 0.75))
                                                .child("Video")
                                                .into_any_element(),
                                        })
                                        .when(!playing, |el| {
                                            el.child(
                                                div()
                                                    .absolute()
                                                    .inset(px(0.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .child(
                                                        div()
                                                            .size(px(64.))
                                                            .rounded(px(32.))
                                                            .bg(hsla(0., 0., 0., 0.55))
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .text_color(rgb(0xffffff))
                                                            .text_size(px(26.))
                                                            .child("▶"),
                                                    ),
                                            )
                                        })
                                        .into_any_element()
                                } else {
                                    match image {
                                        Some(image) => div()
                                            .id("zalo-photo-image")
                                            .absolute()
                                            .left(px(img_left))
                                            .top(px(img_top))
                                            .w(px(dw))
                                            .h(px(dh))
                                            .when(ocr_live, |el| {
                                                el.cursor(CursorStyle::IBeam)
                                                    .on_mouse_down(
                                                        gpui::MouseButton::Left,
                                                        cx.listener(
                                                            move |this,
                                                                  e: &gpui::MouseDownEvent,
                                                                  window,
                                                                  cx| {
                                                                cx.stop_propagation();
                                                                if e.click_count >= 2 {
                                                                    this.ocr_drag = None;
                                                                    this.ocr_selected = (0..this
                                                                        .ocr_boxes
                                                                        .len())
                                                                        .collect();
                                                                    cx.notify();
                                                                    return;
                                                                }
                                                                let (cx_pos, cy_pos) =
                                                                    crate::window_frame::window_to_chrome(
                                                                        window,
                                                                        e.position,
                                                                    );
                                                                let lx =
                                                                    (cx_pos - img_left).clamp(0.0, dw);
                                                                let ly = (cy_pos
                                                                    - (TITLEBAR_H
                                                                        + SUBHEADER_H
                                                                        + img_top))
                                                                    .clamp(0.0, dh);
                                                                this.ocr_drag =
                                                                    Some((lx, ly, lx, ly));
                                                            },
                                                        ),
                                                    )
                                                    .on_mouse_up(
                                                        gpui::MouseButton::Left,
                                                        cx.listener(
                                                            move |this,
                                                                  e: &gpui::MouseUpEvent,
                                                                  window,
                                                                  cx| {
                                                                cx.stop_propagation();
                                                                if this.finish_ocr_drag(
                                                                    e.position,
                                                                    e.modifiers,
                                                                    window,
                                                                    img_left,
                                                                    img_top,
                                                                    dw,
                                                                    dh,
                                                                ) {
                                                                    cx.notify();
                                                                }
                                                            },
                                                        ),
                                                    )
                                                    .on_mouse_down(
                                                        gpui::MouseButton::Right,
                                                        cx.listener(
                                                            move |this,
                                                                  e: &gpui::MouseDownEvent,
                                                                  window,
                                                                  cx| {
                                                                cx.stop_propagation();
                                                                this.ocr_drag = None;
                                                                if this.ocr_selected.is_empty() {
                                                                    let (cx_pos, cy_pos) =
                                                                        crate::window_frame::window_to_chrome(
                                                                            window,
                                                                            e.position,
                                                                        );
                                                                    let lx = (cx_pos - img_left)
                                                                        .clamp(0.0, dw);
                                                                    let ly = (cy_pos
                                                                        - (TITLEBAR_H
                                                                            + SUBHEADER_H
                                                                            + img_top))
                                                                        .clamp(0.0, dh);
                                                                    if let Some(hit) =
                                                                        crate::viewer::ocr_box_at(
                                                                            &this.ocr_boxes,
                                                                            lx,
                                                                            ly,
                                                                            dw,
                                                                            dh,
                                                                        )
                                                                    {
                                                                        this.ocr_selected
                                                                            .insert(hit);
                                                                    }
                                                                }
                                                                this.copy_ocr_selection(cx);
                                                                cx.notify();
                                                            },
                                                        ),
                                                    )
                                            })
                                            .when(!ocr_live, |el| {
                                                el.on_click(cx.listener(
                                                    |this, e: &gpui::ClickEvent, window, cx| {
                                                        cx.stop_propagation();
                                                        let dragged = this.dragged;
                                                        this.dragged = false;
                                                        this.drag = None;
                                                        if dragged {
                                                            return;
                                                        }
                                                        if e.click_count() >= 2 {
                                                            let (sw, sh) = this.stage_size(window);
                                                            let fit = this.fit(sw, sh);
                                                            let actual = (1.0 / fit)
                                                                .clamp(ZOOM_MIN, ZOOM_MAX);
                                                            this.set_zoom(
                                                                if (this.zoom - actual).abs() < 0.05
                                                                {
                                                                    1.0
                                                                } else {
                                                                    actual
                                                                },
                                                            );
                                                            cx.notify();
                                                        }
                                                    },
                                                ))
                                            })
                                            .child(
                                                img(image)
                                                    .w(px(dw))
                                                    .h(px(dh))
                                                    .object_fit(gpui::ObjectFit::Contain),
                                            )
                                            .when(ocr_live, |el| {
                                                el.child(crate::viewer::ocr_marks(
                                                    &ocr_boxes,
                                                    &ocr_selected,
                                                    ocr_drag,
                                                    dw,
                                                    dh,
                                                ))
                                            })
                                            .into_any_element(),
                                        None => div()
                                            .size_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(hsla(0., 0., 1., 0.75))
                                            .child("Đang tải ảnh…")
                                            .into_any_element(),
                                    }
                                })
                                // Floating prev/next circular buttons at right edge of stage
                                .when(self.items.len() > 1, |el| {
                                    el.child(
                                        div()
                                            .absolute()
                                            .right(px(12.))
                                            .top_0()
                                            .bottom_0()
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .justify_center()
                                            .gap(px(10.))
                                            .child(
                                                div()
                                                    .id("zalo-photo-up")
                                                    .size(px(32.))
                                                    .rounded(px(16.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .bg(hsla(0., 0., 0., 0.58))
                                                    .border_1()
                                                    .border_color(hsla(0., 0., 1., 0.1))
                                                    .text_color(if has_prev {
                                                        rgb(0xffffff)
                                                    } else {
                                                        rgb(0x6e_72_77)
                                                    })
                                                    .on_mouse_down(
                                                        gpui::MouseButton::Left,
                                                        |_, _, cx| cx.stop_propagation(),
                                                    )
                                                    .when(has_prev, |el| {
                                                        el.cursor_pointer()
                                                            .hover(|s| s.bg(hsla(0., 0., 0., 0.8)))
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    cx.stop_propagation();
                                                                    this.step(-1, cx);
                                                                    cx.notify();
                                                                },
                                                            ))
                                                    })
                                                    .child(crate::fonts::icon_el(IC_UP, "⌃", 16.)),
                                            )
                                            .child(
                                                div()
                                                    .id("zalo-photo-down")
                                                    .size(px(32.))
                                                    .rounded(px(16.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .bg(hsla(0., 0., 0., 0.58))
                                                    .border_1()
                                                    .border_color(hsla(0., 0., 1., 0.1))
                                                    .text_color(if has_next {
                                                        rgb(0xffffff)
                                                    } else {
                                                        rgb(0x6e_72_77)
                                                    })
                                                    .on_mouse_down(
                                                        gpui::MouseButton::Left,
                                                        |_, _, cx| cx.stop_propagation(),
                                                    )
                                                    .when(has_next, |el| {
                                                        el.cursor_pointer()
                                                            .hover(|s| s.bg(hsla(0., 0., 0., 0.8)))
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    cx.stop_propagation();
                                                                    this.step(1, cx);
                                                                    cx.notify();
                                                                },
                                                            ))
                                                    })
                                                    .child(crate::fonts::icon_el(
                                                        IC_DOWN, "⌄", 16.,
                                                    )),
                                            ),
                                    )
                                })
                                .when_some(
                                    caption.filter(|_| !is_video && !show_blue_pill && !show_status_pill),
                                    |el, text| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .bottom(px(10.))
                                                .left(px(16.))
                                                .right(px(56.))
                                                .text_size(px(13.))
                                                .text_color(hsla(0., 0., 1., 0.9))
                                                .child(text),
                                        )
                                    },
                                )
                                // Floating Electron-style Zalo-Blue OCR callout / selection bar
                                .when(show_blue_pill, |el| {
                                    el.child(
                                        div()
                                            .id("photo-ocr-pill-wrap")
                                            .absolute()
                                            .left_0()
                                            .right_0()
                                            .bottom(px(12.))
                                            .flex()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .id("photo-ocr-blue-pill")
                                                    .flex()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap(px(10.))
                                                    .px(px(14.))
                                                    .py(px(8.))
                                                    .rounded(px(8.))
                                                    .bg(rgb(0x00_68_ff))
                                                    .shadow_lg()
                                                    .text_size(px(13.))
                                                    .text_color(rgb(0xff_ff_ff))
                                                    .on_mouse_down(
                                                        gpui::MouseButton::Left,
                                                        |_, _, cx| cx.stop_propagation(),
                                                    )
                                                    .when(selected_count > 0, |pill| {
                                                        pill.child(
                                                            div()
                                                                .id("photo-ocr-copy-sel")
                                                                .px(px(10.))
                                                                .py(px(3.))
                                                                .rounded(px(5.))
                                                                .bg(hsla(0., 0., 1., 0.22))
                                                                .hover(|s| {
                                                                    s.bg(hsla(0., 0., 1., 0.34))
                                                                })
                                                                .cursor_pointer()
                                                                .font_family(FAMILY_SEMIBOLD)
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        cx.stop_propagation();
                                                                        this.copy_ocr_selection(cx);
                                                                        cx.notify();
                                                                    },
                                                                ))
                                                                .child(if selected_count > 1 {
                                                                    format!(
                                                                        "Sao chép ({selected_count} dòng)"
                                                                    )
                                                                } else {
                                                                    "Sao chép văn bản".to_owned()
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .id("photo-ocr-sel-all")
                                                                .px(px(8.))
                                                                .py(px(3.))
                                                                .rounded(px(5.))
                                                                .hover(|s| {
                                                                    s.bg(hsla(0., 0., 1., 0.16))
                                                                })
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        cx.stop_propagation();
                                                                        this.ocr_selected =
                                                                            (0..this
                                                                                .ocr_boxes
                                                                                .len())
                                                                                .collect();
                                                                        cx.notify();
                                                                    },
                                                                ))
                                                                .child("Chọn tất cả"),
                                                        )
                                                        .child(
                                                            div()
                                                                .id("photo-ocr-clear-sel")
                                                                .size(px(22.))
                                                                .rounded(px(11.))
                                                                .flex()
                                                                .items_center()
                                                                .justify_center()
                                                                .hover(|s| {
                                                                    s.bg(hsla(0., 0., 1., 0.18))
                                                                })
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        cx.stop_propagation();
                                                                        this.ocr_selected.clear();
                                                                        cx.notify();
                                                                    },
                                                                ))
                                                                .child("✕"),
                                                        )
                                                    })
                                                    .when(selected_count == 0, |pill| {
                                                        pill.child(
                                                            "Chọn và sao chép văn bản dễ dàng",
                                                        )
                                                        .child(
                                                            div()
                                                                .id("photo-ocr-copy-all")
                                                                .px(px(8.))
                                                                .py(px(2.))
                                                                .rounded(px(4.))
                                                                .bg(hsla(0., 0., 1., 0.18))
                                                                .hover(|s| {
                                                                    s.bg(hsla(0., 0., 1., 0.30))
                                                                })
                                                                .cursor_pointer()
                                                                .text_size(px(12.))
                                                                .font_family(FAMILY_SEMIBOLD)
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        cx.stop_propagation();
                                                                        this.copy_ocr_selection(cx);
                                                                        cx.notify();
                                                                    },
                                                                ))
                                                                .child("Chép hết"),
                                                        )
                                                        .child(
                                                            div()
                                                                .id("photo-ocr-dismiss-hint")
                                                                .size(px(22.))
                                                                .rounded(px(11.))
                                                                .flex()
                                                                .items_center()
                                                                .justify_center()
                                                                .hover(|s| {
                                                                    s.bg(hsla(0., 0., 1., 0.18))
                                                                })
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        cx.stop_propagation();
                                                                        this.ocr_hint_dismissed =
                                                                            true;
                                                                        cx.notify();
                                                                    },
                                                                ))
                                                                .child("✕"),
                                                        )
                                                    }),
                                            ),
                                    )
                                })
                                // Floating status/install/error pill (scanning, missing OCR runtime, error)
                                .when(show_status_pill, |el| {
                                    let status_text = if ocr_installing {
                                        ocr_install_status.clone().unwrap_or_else(|| {
                                            "Đang tải & cài đặt thư viện OCR...".to_owned()
                                        })
                                    } else if ocr_busy {
                                        "Đang nhận diện văn bản trên ảnh…".to_owned()
                                    } else if missing_ocr {
                                        crate::viewer::ocr_missing_message()
                                    } else {
                                        ocr_err.clone().unwrap_or_default()
                                    };
                                    el.child(
                                        div()
                                            .id("photo-ocr-status-wrap")
                                            .absolute()
                                            .left_0()
                                            .right_0()
                                            .bottom(px(12.))
                                            .flex()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap(px(10.))
                                                    .px(px(14.))
                                                    .py(px(8.))
                                                    .rounded(px(8.))
                                                    .bg(rgb(0x23_25_28))
                                                    .border_1()
                                                    .border_color(hsla(0., 0., 1., 0.14))
                                                    .shadow_lg()
                                                    .text_size(px(12.5))
                                                    .text_color(if ocr_err.is_some() {
                                                        rgb(0xff_b4_b4)
                                                    } else {
                                                        rgb(0xff_ff_ff)
                                                    })
                                                    .on_mouse_down(
                                                        gpui::MouseButton::Left,
                                                        |_, _, cx| cx.stop_propagation(),
                                                    )
                                                    .child(status_text)
                                                    .when(missing_ocr && !ocr_installing, |bar| {
                                                        bar.child(
                                                            div()
                                                                .id("photo-ocr-install")
                                                                .px(px(10.))
                                                                .py(px(4.))
                                                                .rounded(px(5.))
                                                                .bg(rgb(0x00_68_ff))
                                                                .hover(|s| s.bg(rgb(0x00_5a_e0)))
                                                                .cursor_pointer()
                                                                .font_family(FAMILY_SEMIBOLD)
                                                                .text_color(rgb(0xff_ff_ff))
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        cx.stop_propagation();
                                                                        this.start_ocr_install(cx);
                                                                        cx.notify();
                                                                    },
                                                                ))
                                                                .child("Tải & cài OCR (~18,6 MB)"),
                                                        )
                                                    })
                                                    .when(!ocr_busy && !ocr_installing, |bar| {
                                                        bar.child(
                                                            div()
                                                                .id("photo-ocr-err-close")
                                                                .size(px(22.))
                                                                .rounded(px(11.))
                                                                .flex()
                                                                .items_center()
                                                                .justify_center()
                                                                .hover(|s| {
                                                                    s.bg(hsla(0., 0., 1., 0.16))
                                                                })
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        cx.stop_propagation();
                                                                        this.ocr_result = None;
                                                                        this.ocr_active = false;
                                                                        cx.notify();
                                                                    },
                                                                ))
                                                                .child("✕"),
                                                        )
                                                    }),
                                            ),
                                    )
                                })
                                // Brief auto-dismissing toast at top-center of stage
                                .when_some(note.clone(), |el, text| {
                                    el.child(
                                        div()
                                            .absolute()
                                            .left_0()
                                            .right_0()
                                            .top(px(14.))
                                            .flex()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .px(px(14.))
                                                    .py(px(6.))
                                                    .rounded(px(16.))
                                                    .bg(hsla(0., 0., 0., 0.78))
                                                    .border_1()
                                                    .border_color(hsla(0., 0., 1., 0.12))
                                                    .shadow_md()
                                                    .text_size(px(12.5))
                                                    .text_color(rgb(0xffffff))
                                                    .child(text),
                                            ),
                                    )
                                }),
                        )
                        .when(rail_open, |el| {
                            el.child(
                                div()
                                    .id("zalo-photo-strip")
                                    .w(px(RAIL_W))
                                    .flex_none()
                                    .h_full()
                                    .flex()
                                    .flex_col()
                                    .bg(rgb(RAIL_BG))
                                    .border_l_1()
                                    .border_color(hsla(0., 0., 1., 0.06))
                                    .overflow_y_scroll()
                                    .track_scroll(&self.film_scroll)
                                    .children(rows),
                            )
                        }),
                )
                // 4. Electron-style 3-zone bottom bar
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(FOOTER_H))
                        .px(px(14.))
                        .bg(rgb(BAR_BG))
                        .border_t_1()
                        .border_color(hsla(0., 0., 1., 0.06))
                        .when(frame.bl, |el| el.rounded_bl(frame.radius))
                        .when(frame.br, |el| el.rounded_br(frame.radius))
                        // Left: sender avatar + name + time
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(10.))
                                .w(px(230.))
                                .min_w(px(0.))
                                .child(match sender_avatar {
                                    Some(av) => img(av)
                                        .size(px(36.))
                                        .flex_none()
                                        .rounded(px(18.))
                                        .object_fit(gpui::ObjectFit::Cover)
                                        .into_any_element(),
                                    None => div()
                                        .size(px(36.))
                                        .flex_none()
                                        .rounded(px(18.))
                                        .bg(rgb(0x0068ff))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(rgb(0xffffff))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_size(px(14.))
                                        .child(initial)
                                        .into_any_element(),
                                })
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(
                                            div()
                                                .text_size(px(13.))
                                                .font_family(FAMILY_SEMIBOLD)
                                                .text_color(rgb(0xffffff))
                                                .truncate()
                                                .child(sender),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.5))
                                                .text_color(hsla(0., 0., 1., 0.65))
                                                .truncate()
                                                .child(when),
                                        ),
                                ),
                        )
                        // Center: Electron action groups `[Share Download] | [Rotate OCR] | [ZoomIn ZoomOut]`
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(4.))
                                .child(
                                    icon_btn("zalo-photo-share", IC_SHARE, "Chia sẻ", false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.share_current(cx);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    icon_btn("zalo-photo-save", IC_DOWNLOAD, "Lưu về máy", false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.save_current(cx);
                                            cx.notify();
                                        })),
                                )
                                .child(bar_divider())
                                .child(
                                    icon_btn("zalo-photo-rot-r", IC_ROTATE_L, "Xoay ảnh", false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.rotate(1, cx);
                                            cx.notify();
                                        })),
                                )
                                .when(can_ocr, |el| {
                                    el.child(
                                        icon_btn(
                                            "zalo-photo-ocr",
                                            IC_OCR,
                                            if ocr_busy {
                                                "Đang quét văn bản…"
                                            } else if ocr_live {
                                                "Tắt chọn văn bản trên ảnh"
                                            } else {
                                                "Chọn và sao chép văn bản (OCR)"
                                            },
                                            ocr_live || ocr_busy,
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.start_ocr(cx);
                                            cx.notify();
                                        })),
                                    )
                                })
                                .child(bar_divider())
                                .child(
                                    icon_btn("zalo-photo-in", IC_ZOOM_IN, "Phóng to", false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            let z = this.zoom;
                                            this.set_zoom(z * 1.25);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    icon_btn("zalo-photo-out", IC_ZOOM_OUT, "Thu nhỏ", false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            let z = this.zoom;
                                            this.set_zoom(z / 1.25);
                                            cx.notify();
                                        })),
                                )
                                .when(zoomed, |el| {
                                    el.child(
                                        div()
                                            .id("zalo-photo-fit")
                                            .ml(px(4.))
                                            .px(px(8.))
                                            .h(px(26.))
                                            .rounded(px(5.))
                                            .bg(hsla(0., 0., 1., 0.1))
                                            .hover(|s| s.bg(hsla(0., 0., 1., 0.18)))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(rgb(0xffffff))
                                            .tooltip(|_, cx| {
                                                crate::widgets::tip("Khôi phục vừa khung", cx)
                                            })
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                |_, _, cx| cx.stop_propagation(),
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.set_zoom(1.0);
                                                cx.notify();
                                            }))
                                            .child(format!("{percent:.0}%")),
                                    )
                                }),
                        )
                        // Right: Copy image + Toggle filmstrip sidebar
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_end()
                                .gap(px(4.))
                                .w(px(230.))
                                .child(
                                    icon_btn("zalo-photo-copy", IC_COPY, "Sao chép ảnh", false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.copy_current(cx);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    icon_btn(
                                        "zalo-photo-toggle-rail",
                                        crate::fonts::ic::HDR_RIGHT_BAR,
                                        if rail_open {
                                            "Ẩn danh sách ảnh"
                                        } else {
                                            "Hiện danh sách ảnh"
                                        },
                                        rail_open,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.rail_open = !this.rail_open;
                                        cx.notify();
                                    })),
                                ),
                        ),
                ),
        )
    }
}

/// Filmstrip date: "Today", "Yesterday", or `dd/mm` in Vietnam time.
#[must_use]
pub(crate) fn filmstrip_day(at_ms: i64, now_ms: i64) -> String {
    if at_ms <= 0 {
        return String::new();
    }
    match crate::data::vn_day(now_ms) - crate::data::vn_day(at_ms) {
        0 => crate::color::str_vi("STR_TODAY", "Hôm nay").to_owned(),
        1 => crate::color::str_vi("STR_YESTERDAY_RAW", "Hôm qua").to_owned(),
        _ => {
            let (_, month, day) = crate::data::vn_ymd(at_ms);
            format!("{day:02}/{month:02}")
        }
    }
}

fn clamp_pan(pan_x: f32, pan_y: f32, dw: f32, dh: f32, stage_w: f32, stage_h: f32) -> (f32, f32) {
    let max_x = ((dw - stage_w) * 0.5).max(0.0);
    let max_y = ((dh - stage_h) * 0.5).max(0.0);
    (pan_x.clamp(-max_x, max_x), pan_y.clamp(-max_y, max_y))
}

fn video_file(msg: &ChatMsg) -> Option<PathBuf> {
    let href = PathBuf::from(&msg.href);
    msg.media_path
        .clone()
        .filter(|p| p.is_file())
        .or_else(|| href.is_file().then_some(href))
        .or_else(|| {
            crate::media::cache_path_for(&msg.href)
                .map(|p| p.with_extension("mp4"))
                .filter(|p| p.is_file())
        })
}

/// Native in-process MP4 H.264 frame pump using `mp4` + `openh264` (zero `ffmpeg`/`ffplay`/`kill`).
fn spawn_mp4_frames(
    path: &Path,
    stop: Arc<AtomicBool>,
    frames: Arc<Mutex<VecDeque<Arc<RenderImage>>>>,
) -> bool {
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name("zalo-video".into())
        .spawn(move || {
            let Some((mut reader, track_id, sps, pps, timescale, sample_count)) =
                crate::video::open_h264_mp4(&path)
            else {
                stop.store(true, Ordering::Relaxed);
                return;
            };
            let Ok(mut decoder) = openh264::decoder::Decoder::new() else {
                stop.store(true, Ordering::Relaxed);
                return;
            };
            use openh264::formats::YUVSource;
            let started = std::time::Instant::now();
            for idx in 1..=sample_count {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(Some(sample)) = reader.read_sample(track_id, idx) else {
                    break;
                };
                // Pace frame delivery to match real MP4 presentation time.
                let pts_ms = sample
                    .start_time
                    .saturating_mul(1_000)
                    / u64::from(timescale);
                let elapsed_ms = started.elapsed().as_millis() as u64;
                if pts_ms > elapsed_ms + 5 {
                    let wait = (pts_ms - elapsed_ms).min(250);
                    std::thread::sleep(std::time::Duration::from_millis(wait));
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                }
                let annex_b = if idx == 1 || sample.is_sync {
                    crate::video::avcc_to_annex_b(&sample.bytes, Some(&sps), Some(&pps))
                } else {
                    crate::video::avcc_to_annex_b(&sample.bytes, None, None)
                };
                let Ok(Some(yuv)) = decoder.decode(&annex_b) else {
                    continue;
                };
                let (w, h) = yuv.dimensions();
                if w == 0 || h == 0 {
                    continue;
                }
                let bgra = crate::call_video::yuv_to_bgra(&yuv, w, h);
                let (sw, sh) = (w as u32, h as u32);
                let max_edge = 960_u32;
                let (dw, dh, out_bgra) = if sw.max(sh) > max_edge {
                    let (nw, nh) = if sw >= sh {
                        (max_edge, ((u64::from(sh) * u64::from(max_edge)) / u64::from(sw)).max(2) as u32)
                    } else {
                        (((u64::from(sw) * u64::from(max_edge)) / u64::from(sh)).max(2) as u32, max_edge)
                    };
                    (nw, nh, crate::call_video::scale_bgra_bilinear(&bgra, sw, sh, nw, nh))
                } else {
                    (sw, sh, bgra)
                };
                if let Some(buf) = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(dw, dh, out_bgra) {
                    let image = Arc::new(RenderImage::new([image::Frame::new(buf)]));
                    if let Ok(mut queue) = frames.lock() {
                        queue.push_back(image);
                        while queue.len() > 3 {
                            queue.pop_front();
                        }
                    }
                }
            }
            stop.store(true, Ordering::Relaxed);
        })
        .is_ok()
}

impl Drop for PhotoView {
    fn drop(&mut self) {
        self.stop_video();
    }
}

fn decode_photo(path: Option<PathBuf>, url: &str, turns: u8) -> Option<Arc<RenderImage>> {
    let file = path.or_else(|| crate::media::cache_path_for(url).filter(|p| p.is_file()))?;
    let bytes = std::fs::read(file).ok()?;
    let rgba = crate::media::decode_rgba(&bytes, 2400)?;
    let rgba = crate::viewer::rotate_turns(rgba, turns);
    Some(Arc::new(RenderImage::new([image::Frame::new(
        crate::media::rgba_to_bgra(rgba),
    )])))
}

fn photo_file(msg: &ChatMsg) -> Option<PathBuf> {
    crate::media::local_media_file(msg.media_path.as_deref(), &msg.media_url, &msg.href)
}

fn copy_photo_bytes(msg: &ChatMsg) -> Result<(gpui::ImageFormat, Vec<u8>), String> {
    let path = photo_file(msg).ok_or_else(|| "Ảnh chưa có trên máy để copy".to_owned())?;
    crate::clip::encode_image_file(&path)
}

fn save_photo_file(msg: &ChatMsg, dir: &Path) -> Result<String, String> {
    if msg.kind == MsgKind::Video {
        let src = video_file(msg).ok_or_else(|| "Video chưa có trên máy để lưu".to_owned())?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let dest = dir.join(format!("zalo-{}.mp4", msg.id));
        std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
        return Ok(dest.display().to_string());
    }
    let src = photo_file(msg).ok_or_else(|| "Ảnh chưa có trên máy để lưu".to_owned())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let ext = std::fs::read(&src)
        .ok()
        .and_then(|b| image::guess_format(&b).ok())
        .and_then(|f| f.extensions_str().first().copied())
        .unwrap_or("jpg");
    let dest = dir.join(format!("zalo-{}.{ext}", msg.id));
    std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
    Ok(dest.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_matches_the_electron_popup() {
        assert_eq!(photo_window_title("Team Group"), "Zalo Photo");
        assert_eq!(photo_window_title(""), "Zalo Photo");
    }

    #[test]
    fn gallery_source_is_a_local_file_or_an_http_url() {
        let mut msg = ChatMsg::default();
        assert!(http_media(&msg).is_empty());
        assert!(!has_gallery_source(&msg));
        msg.href = "https://cdn.example/a.jpg".into();
        assert_eq!(http_media(&msg), "https://cdn.example/a.jpg");
        assert!(has_gallery_source(&msg));
        msg.media_url = "http://cdn.example/b.jpg".into();
        assert_eq!(http_media(&msg), "http://cdn.example/b.jpg");
        msg.media_url.clear();
        msg.href = "not a url".into();
        assert!(!has_gallery_source(&msg));
    }

    #[test]
    fn filmstrip_groups_today_yesterday_and_a_date() {
        let now = 1_700_000_000_000i64;
        assert_eq!(
            filmstrip_day(now, now),
            crate::color::str_vi("STR_TODAY", "Hôm nay")
        );
        assert_eq!(
            filmstrip_day(now - 86_400_000, now),
            crate::color::str_vi("STR_YESTERDAY_RAW", "Hôm qua")
        );
        let older = now - 3 * 86_400_000;
        let (_, month, day) = crate::data::vn_ymd(older);
        assert_eq!(filmstrip_day(older, now), format!("{day:02}/{month:02}"));
        assert!(filmstrip_day(0, now).is_empty());
    }
}
