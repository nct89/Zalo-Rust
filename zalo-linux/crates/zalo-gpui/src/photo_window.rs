//! Separate photo window, the Electron popup the live app titles `Zalo Photo`.
//!
//! The page matches that window: conversation name centered in the header,
//! the picture on a dark stage, a date-grouped filmstrip on the right,
//! up/down chevrons, and a footer with the sender, time and icon buttons.
//! A video uses the same chrome and plays the file already on disk.
//! Clicking a photo or video in chat opens this window instead of covering
//! the conversation or jumping straight to the system player.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::data::ChatMsg;
use crate::fonts::FAMILY_SEMIBOLD;
use crate::media::MsgKind;
use gpui::{
    div, hsla, img, prelude::*, px, rgb, size, App, Bounds, Context, CursorStyle, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ParentElement, Render, RenderImage, ScrollHandle,
    ScrollWheelEvent, StatefulInteractiveElement, Styled, Window, WindowBounds, WindowDecorations,
    WindowOptions,
};

const HEADER_H: f32 = 44.0;
const FOOTER_H: f32 = 64.0;
const RAIL_W: f32 = 132.0;
const CHEVRON_W: f32 = 40.0;
const IC_SHARE: char = '\u{ED29}';
const IC_DOWNLOAD: char = '\u{EAE1}';
const IC_ROTATE_L: char = '\u{ED0E}';
const IC_ZOOM_IN: char = '\u{EE16}';
const IC_ZOOM_OUT: char = '\u{EE18}';
const IC_ZOOM_FIT: char = '\u{EE17}';
const IC_COPY: char = '\u{EAC5}';
const IC_OCR: char = '\u{ED0F}';
const IC_UP: char = '\u{EA91}';
const IC_DOWN: char = '\u{EA8B}';

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 6.0;
const STAGE_BG: u32 = 0x3a3c3d;
const BAR_BG: u32 = 0x2e3031;

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
pub(crate) fn open(
    cx: &mut App,
    conv_name: String,
    items: Vec<ChatMsg>,
    current_id: u64,
    download_dir: PathBuf,
    session_dir: Option<PathBuf>,
    start_ocr: bool,
    mute_video: bool,
) -> bool {
    if items.is_empty() {
        return false;
    }
    let title = photo_window_title(&conv_name);
    let bounds = Bounds::centered(None, size(px(1120.), px(720.)), cx);
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
                    download_dir,
                    session_dir,
                    start_ocr,
                    mute_video,
                )
            });
            let focus = view.focus_handle(cx);
            window.focus(&focus);
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
    image: Option<Arc<RenderImage>>,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    turns: u8,
    drag: Option<(f32, f32, f32, f32)>,
    dragged: bool,
    ocr_busy: bool,
    ocr_result: Option<Result<String, String>>,
    /// Line boxes from the last successful scan, fractions of the image.
    ocr_boxes: Vec<crate::viewer::OcrBox>,
    ocr_installing: bool,
    ocr_install_status: Option<String>,
    note: Option<String>,
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
    fn new(
        cx: &mut Context<Self>,
        conv_name: String,
        items: Vec<ChatMsg>,
        current_id: u64,
        download_dir: PathBuf,
        session_dir: Option<PathBuf>,
        start_ocr: bool,
        mute_video: bool,
    ) -> Self {
        let index = items.iter().position(|m| m.id == current_id).unwrap_or(0);
        let thumbs = vec![None; items.len()];
        let mut this = Self {
            focus: cx.focus_handle(),
            conv_name,
            items,
            index,
            image: None,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            turns: 0,
            drag: None,
            dragged: false,
            ocr_busy: false,
            ocr_result: None,
            ocr_boxes: Vec::new(),
            ocr_installing: false,
            ocr_install_status: None,
            note: None,
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
        cx.spawn(async move |this, cx| {
            let image = cx
                .background_executor()
                .spawn(async move { decode_photo(path, &url, turns) })
                .await;
            let _ = this.update(cx, |view, cx| {
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
        self.ocr_result = None;
        self.ocr_boxes.clear();
        self.ocr_busy = false;
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
        self.ocr_result = None;
        self.ocr_boxes.clear();
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
        // Original `OCR_TOGGLE`: a finished scan shows or hides the overlay.
        // Copy stays on the "Copy Text" control, not on the scan itself.
        if self.ocr_result.is_some() {
            self.ocr_result = None;
            self.ocr_boxes.clear();
            return;
        }
        let msg = self.current().clone();
        if !matches!(msg.kind, MsgKind::Photo | MsgKind::Doodle | MsgKind::Gif) {
            return;
        }
        self.ocr_busy = true;
        self.ocr_result = None;
        self.ocr_boxes.clear();
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
                            Some(d) if url.starts_with("https://") => {
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
                            view.note = Some("Đã quét chữ trên ảnh".to_owned());
                            view.ocr_boxes = scan.boxes;
                            view.ocr_result = Some(Ok(scan.text));
                        }
                        Err(err) => {
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

    fn share_current(&mut self, _cx: &mut Context<Self>) {
        if self.current().kind == MsgKind::Video {
            self.note = Some("Chia sẻ video từ khung chat".to_owned());
            return;
        }
        push_share(self.current().clone());
        self.note = Some("Đã mở hộp chuyển tiếp ở cửa sổ chính".to_owned());
    }

    fn copy_current(&mut self, cx: &mut Context<Self>) {
        let msg = self.current().clone();
        if msg.kind == MsgKind::Video {
            self.note = Some("Video không copy được như ảnh".to_owned());
            return;
        }
        match copy_photo_bytes(&msg) {
            Ok((format, bytes)) => {
                crate::clip::copy_image(cx, format, bytes);
                self.note = Some("Đã copy hình ảnh".to_owned());
            }
            Err(err) => self.note = Some(err),
        }
    }

    fn save_current(&mut self) {
        let msg = self.current().clone();
        match save_photo_file(&msg, &self.download_dir) {
            Ok(dest) => self.note = Some(format!("Đã lưu {dest}")),
            Err(err) => self.note = Some(err),
        }
    }

    fn load_thumbs(&mut self, cx: &mut Context<Self>) {
        let session = self.session_dir.clone();
        let jobs: Vec<crate::fetch::Job> = self
            .items
            .iter()
            .map(|msg| crate::fetch::Job {
                msg_id: msg.id,
                path: msg.media_path.clone().filter(|path| path.is_file()),
                url: http_media(msg),
                max_edge: 160,
            })
            .collect();
        let n = jobs.len();
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_executor()
                .spawn(async move { crate::fetch::run_batch(session.as_deref(), jobs) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.items.len() == n {
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
                    }
                    view.film_pinned = usize::MAX;
                    if stage_missing {
                        view.reload(cx);
                    }
                }
                cx.notify();
            });
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

    fn on_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match key {
            "escape" => {
                window.remove_window();
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
                let (sw, sh) = stage_size(window);
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
        let (stage_w, stage_h) = stage_size(window);
        let fit = self.fit(stage_w, stage_h);
        let zoom = self.zoom;
        let (pw, ph) = self.pixels();
        let (dw, dh) = ((pw * fit * zoom).max(1.0), (ph * fit * zoom).max(1.0));
        let percent = (fit * zoom * 100.0).round().clamp(1.0, 9999.0);
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
        let can_pan = !is_video && (dw > stage_w + 1.0 || dh > stage_h + 1.0);
        let dragging = self.drag.is_some();
        let ocr_busy = self.ocr_busy;
        let ocr_result = self.ocr_result.clone();
        let ocr_boxes = self.ocr_boxes.clone();
        let ocr_installing = self.ocr_installing;
        let ocr_install_status = self.ocr_install_status.clone();
        let can_ocr = matches!(msg.kind, MsgKind::Photo | MsgKind::Doodle | MsgKind::Gif);
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
        let initial = sender.chars().next().unwrap_or('?').to_string();
        let when = if msg.at_ms > 0 {
            crate::data::day_label(msg.at_ms, crate::data::now_ms())
        } else {
            msg.time.clone()
        };
        self.pin_film();

        let icon_btn = |id: &'static str, code: char, label: &'static str| {
            div()
                .id(id)
                .size(px(32.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .text_color(rgb(0xffffff))
                .hover(|st| st.bg(hsla(0., 0., 1., 0.12)))
                .tooltip(move |_, cx| crate::widgets::tip(label, cx))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(crate::fonts::icon_el(code, "•", 18.))
        };

        let now = crate::data::now_ms();
        let metas: Vec<(u64, i64, bool)> = self
            .items
            .iter()
            .map(|m| (m.id, m.at_ms, m.kind == MsgKind::Video))
            .collect();
        let thumbs = self.thumbs.clone();
        let mut prev_day = String::new();
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
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
                                .pt(px(8.))
                                .pb(px(4.))
                                .text_size(px(12.))
                                .text_color(hsla(0., 0., 1., 0.72))
                                .child(label.clone()),
                        )
                    })
                    .child({
                        let mut cell = div()
                            .mb(px(6.))
                            .w(px(108.))
                            .h(px(72.))
                            .rounded(px(4.))
                            .overflow_hidden()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(rgb(0x1a1c1d))
                            .border_2()
                            .border_color(if selected {
                                rgb(0xffffff).into()
                            } else {
                                hsla(0., 0., 1., 0.)
                            });
                        cell = match thumb {
                            Some(t) => cell.child(
                                img(t)
                                    .w(px(108.))
                                    .h(px(72.))
                                    .object_fit(gpui::ObjectFit::Cover),
                            ),
                            None => cell.bg(rgb(0x5c6166)).child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(hsla(0., 0., 1., 0.92))
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

        let frame = crate::window_frame::CsdCorners::from_window(window);
        crate::window_frame::wrap_csd(
            window,
            div()
                .id("zalo-photo")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
                    let mods = &e.keystroke.modifiers;
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
                .child(
                    div()
                        .id("zalo-photo-header")
                        .relative()
                        .flex()
                        .flex_none()
                        .h(px(HEADER_H))
                        .items_center()
                        .justify_center()
                        .px(px(140.))
                        .bg(rgb(BAR_BG))
                        .when(frame.tl, |el| el.rounded_tl(frame.radius))
                        .when(frame.tr, |el| el.rounded_tr(frame.radius))
                        .text_size(px(15.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(0xffffff))
                        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                            window.start_window_move();
                        })
                        .on_click(|event, window, _| {
                            if event.click_count() >= 2 {
                                window.zoom_window();
                            }
                        })
                        .child(div().truncate().child(header))
                        .child(
                            div()
                                .absolute()
                                .right_0()
                                .top_0()
                                .h_full()
                                .flex()
                                .flex_row()
                                .items_center()
                                .child(photo_caption_btn("zalo-photo-min", "−", false).on_click(
                                    cx.listener(|_, _, window, cx| {
                                        cx.stop_propagation();
                                        window.minimize_window();
                                    }),
                                ))
                                .child(photo_caption_btn("zalo-photo-max", "□", false).on_click(
                                    cx.listener(|_, _, window, cx| {
                                        cx.stop_propagation();
                                        window.zoom_window();
                                    }),
                                ))
                                .child(photo_caption_btn("zalo-photo-close", "×", true).on_click(
                                    cx.listener(|_, _, window, cx| {
                                        cx.stop_propagation();
                                        window.remove_window();
                                    }),
                                )),
                        ),
                )
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
                                        // The filmstrip is a sibling. A listener on the
                                        // window root also saw that wheel and zoomed.
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
                                        cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
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
                                        |this, e: &gpui::MouseMoveEvent, window, cx| {
                                            let Some((ox, oy, px0, py0)) = this.drag else {
                                                return;
                                            };
                                            let dx = f32::from(e.position.x) - ox;
                                            let dy = f32::from(e.position.y) - oy;
                                            if dx.abs() + dy.abs() > 3.0 {
                                                this.dragged = true;
                                            }
                                            let (sw, sh) = stage_size(window);
                                            let fit = this.fit(sw, sh);
                                            let (pw, ph) = this.pixels();
                                            let dw = (pw * fit * this.zoom).max(1.0);
                                            let dh = (ph * fit * this.zoom).max(1.0);
                                            let (nx, ny) =
                                                clamp_pan(px0 + dx, py0 + dy, dw, dh, sw, sh);
                                            this.pan_x = nx;
                                            this.pan_y = ny;
                                            cx.notify();
                                        },
                                    ))
                                    .on_mouse_up(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _: &gpui::MouseUpEvent, _, _| {
                                            this.drag = None;
                                        }),
                                    )
                                    .on_mouse_up_out(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _: &gpui::MouseUpEvent, _, _| {
                                            this.drag = None;
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
                                            .on_click(cx.listener(
                                                |this, e: &gpui::ClickEvent, window, cx| {
                                                    cx.stop_propagation();
                                                    let dragged = this.dragged;
                                                    this.dragged = false;
                                                    this.drag = None;
                                                    if dragged {
                                                        return;
                                                    }
                                                    if e.click_count() >= 2 {
                                                        let (sw, sh) = stage_size(window);
                                                        let fit = this.fit(sw, sh);
                                                        let actual =
                                                            (1.0 / fit).clamp(ZOOM_MIN, ZOOM_MAX);
                                                        this.set_zoom(
                                                            if (this.zoom - actual).abs() < 0.05 {
                                                                1.0
                                                            } else {
                                                                actual
                                                            },
                                                        );
                                                        cx.notify();
                                                    }
                                                },
                                            ))
                                            .w(px(dw))
                                            .h(px(dh))
                                            .child(
                                                img(image)
                                                    .w(px(dw))
                                                    .h(px(dh))
                                                    .object_fit(gpui::ObjectFit::Contain),
                                            )
                                            .when(!ocr_boxes.is_empty(), |el| {
                                                el.child(crate::viewer::ocr_marks(
                                                    &ocr_boxes, dw, dh,
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
                                .when_some(caption.filter(|_| !is_video), |el, text| {
                                    el.child(
                                        div()
                                            .absolute()
                                            .bottom(px(8.))
                                            .left(px(16.))
                                            .right(px(16.))
                                            .text_size(px(13.))
                                            .text_color(hsla(0., 0., 1., 0.9))
                                            .child(text),
                                    )
                                })
                                .when_some(note.clone(), |el, text| {
                                    el.child(
                                        div()
                                            .absolute()
                                            .left(px(12.))
                                            .right(px(12.))
                                            .bottom(px(8.))
                                            .px(px(10.))
                                            .py(px(4.))
                                            .rounded(px(4.))
                                            .bg(hsla(0., 0., 0., 0.72))
                                            .text_size(px(12.))
                                            .text_color(rgb(0xffffff))
                                            .child(text),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .w(px(CHEVRON_W))
                                .flex()
                                .flex_none()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap(px(10.))
                                .child(
                                    div()
                                        .id("zalo-photo-up")
                                        .size(px(28.))
                                        .rounded(px(14.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(hsla(0., 0., 0., 0.55))
                                        .text_color(if has_prev {
                                            rgb(0xffffff)
                                        } else {
                                            rgb(0x8a8e93)
                                        })
                                        .when(has_prev, |el| {
                                            el.cursor_pointer().on_click(cx.listener(
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
                                        .size(px(28.))
                                        .rounded(px(14.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(hsla(0., 0., 0., 0.55))
                                        .text_color(if has_next {
                                            rgb(0xffffff)
                                        } else {
                                            rgb(0x8a8e93)
                                        })
                                        .when(has_next, |el| {
                                            el.cursor_pointer().on_click(cx.listener(
                                                |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.step(1, cx);
                                                    cx.notify();
                                                },
                                            ))
                                        })
                                        .child(crate::fonts::icon_el(IC_DOWN, "⌄", 16.)),
                                ),
                        )
                        .child(
                            div()
                                .id("zalo-photo-strip")
                                .w(px(RAIL_W))
                                .flex_none()
                                .h_full()
                                .flex()
                                .flex_col()
                                .bg(rgb(0x323436))
                                .overflow_y_scroll()
                                .track_scroll(&self.film_scroll)
                                .children(rows),
                        ),
                )
                .when(ocr_busy || ocr_installing || ocr_result.is_some(), |el| {
                    let missing_ocr = matches!(
                        &ocr_result,
                        Some(Err(err)) if err == &crate::viewer::ocr_missing_message()
                    );
                    let copy_text = ocr_result.as_ref().and_then(|r| r.as_ref().ok()).cloned();
                    let is_err = matches!(ocr_result, Some(Err(_)));
                    let text = if ocr_installing {
                        ocr_install_status
                            .clone()
                            .unwrap_or_else(|| "Đang tải & cài đặt thư viện OCR...".to_owned())
                    } else if ocr_busy {
                        "Đang quét chữ…".to_owned()
                    } else {
                        match &ocr_result {
                            Some(Ok(t)) => t.clone(),
                            Some(Err(t)) => t.clone(),
                            None => String::new(),
                        }
                    };
                    el.child(
                        div()
                            .id("photo-ocr-panel")
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .px(px(16.))
                            .py(px(8.))
                            .bg(rgb(0x232526))
                            .border_t_1()
                            .border_color(hsla(0., 0., 1., 0.12))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(hsla(0., 0., 1., 0.8))
                                            .child("Văn bản trong ảnh (OCR)"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .gap(px(8.))
                                            .when(missing_ocr && !ocr_installing, |row| {
                                                row.child(
                                                    div()
                                                        .id("photo-ocr-install")
                                                        .px(px(10.))
                                                        .py(px(3.))
                                                        .rounded(px(4.))
                                                        .bg(rgb(0x00_68_ff))
                                                        .hover(|s| s.bg(rgb(0x00_5a_e0)))
                                                        .cursor_pointer()
                                                        .text_size(px(12.))
                                                        .font_family(FAMILY_SEMIBOLD)
                                                        .text_color(rgb(0xff_ff_ff))
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            cx.stop_propagation();
                                                            this.start_ocr_install(cx);
                                                            cx.notify();
                                                        }))
                                                        .child("Tải & cài OCR từ R2 (~18,6 MB)"),
                                                )
                                            })
                                            .when_some(copy_text, |row, text| {
                                                row.child(
                                                    div()
                                                        .id("photo-ocr-copy")
                                                        .px(px(10.))
                                                        .py(px(3.))
                                                        .rounded(px(4.))
                                                        .bg(hsla(0., 0., 1., 0.14))
                                                        .hover(|s| s.bg(hsla(0., 0., 1., 0.24)))
                                                        .cursor_pointer()
                                                        .text_size(px(12.))
                                                        .text_color(rgb(0xff_ff_ff))
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            cx.stop_propagation();
                                                            crate::clip::copy_text(cx, text.clone());
                                                            this.note = Some("Đã sao chép văn bản OCR".to_owned());
                                                            cx.notify();
                                                        }))
                                                        .child("Chép chữ"),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .id("photo-ocr-hide")
                                                    .px(px(8.))
                                                    .py(px(3.))
                                                    .rounded(px(4.))
                                                    .bg(hsla(0., 0., 1., 0.1))
                                                    .hover(|s| s.bg(hsla(0., 0., 1., 0.2)))
                                                    .cursor_pointer()
                                                    .text_size(px(12.))
                                                    .text_color(hsla(0., 0., 1., 0.85))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        cx.stop_propagation();
                                                        this.ocr_result = None;
                                                        this.ocr_boxes.clear();
                                                        cx.notify();
                                                    }))
                                                    .child("Ẩn"),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .id("photo-ocr-body")
                                    .max_h(px(110.))
                                    .overflow_y_scroll()
                                    .text_size(px(13.))
                                    .text_color(if is_err && !missing_ocr {
                                        rgb(0xff_b4_b4)
                                    } else {
                                        rgb(0xff_ff_ff)
                                    })
                                    .child(text),
                            ),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .h(px(FOOTER_H))
                        .px(px(12.))
                        .bg(rgb(BAR_BG))
                        .when(frame.bl, |el| el.rounded_bl(frame.radius))
                        .when(frame.br, |el| el.rounded_br(frame.radius))
                        .child(
                            div()
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
                                .child(initial),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.))
                                .w(px(180.))
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
                                        .text_size(px(12.))
                                        .text_color(hsla(0., 0., 1., 0.7))
                                        .truncate()
                                        .child(when),
                                ),
                        )
                        .child(div().flex_1())
                        .child(icon_btn("zalo-photo-share", IC_SHARE, "Chia sẻ").on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.share_current(cx);
                                cx.notify();
                            }),
                        ))
                        .child(
                            icon_btn("zalo-photo-save", IC_DOWNLOAD, "Lưu về máy").on_click(
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.save_current();
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(
                            icon_btn("zalo-photo-rot-r", IC_ROTATE_L, "Xoay phải").on_click(
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.rotate(1, cx);
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(icon_btn("zalo-photo-out", IC_ZOOM_OUT, "Thu nhỏ").on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                let z = this.zoom;
                                this.set_zoom(z / 1.25);
                                cx.notify();
                            }),
                        ))
                        .child(
                            div()
                                .min_w(px(44.))
                                .text_size(px(12.))
                                .text_color(rgb(0xffffff))
                                .child(format!("{percent:.0}%")),
                        )
                        .child(icon_btn("zalo-photo-in", IC_ZOOM_IN, "Phóng to").on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                let z = this.zoom;
                                this.set_zoom(z * 1.25);
                                cx.notify();
                            }),
                        ))
                        .child(
                            icon_btn("zalo-photo-fit", IC_ZOOM_FIT, "Vừa khung").on_click(
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.set_zoom(1.0);
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(
                            icon_btn("zalo-photo-copy", IC_COPY, "Copy").on_click(cx.listener(
                                |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.copy_current(cx);
                                    cx.notify();
                                },
                            )),
                        )
                        .when(can_ocr, |el| {
                            el.child(
                                icon_btn(
                                    "zalo-photo-ocr",
                                    IC_OCR,
                                    if ocr_busy {
                                        "Đang quét…"
                                    } else {
                                        "Quét chữ"
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.start_ocr(cx);
                                        cx.notify();
                                    },
                                )),
                            )
                        }),
                ),
        )
    }
}
fn stage_size(window: &Window) -> (f32, f32) {
    let vp = window.viewport_size();
    (
        (f32::from(vp.width) - RAIL_W - CHEVRON_W).max(200.0),
        (f32::from(vp.height) - HEADER_H - FOOTER_H).max(80.0),
    )
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

fn photo_caption_btn(
    id: &'static str,
    glyph: &'static str,
    close: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(46.))
        .h(px(HEADER_H))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(16.))
        .text_color(rgb(0xffffff))
        .cursor_pointer()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .hover(move |style| style.bg(rgb(if close { 0xc31818 } else { 0x4a5054 })))
        .child(glyph)
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
