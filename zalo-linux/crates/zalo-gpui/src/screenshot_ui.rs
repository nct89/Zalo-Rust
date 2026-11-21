//! Built-in native GPUI region screenshot overlay for X11 (`x11rb` + RandR).
//!
//! Mirrors Zalo PC's screenshot UX without requiring external CLI region tools
//! (`spectacle` / `flameshot` / `gnome-screenshot`): captures the active physical
//! monitor at native 1:1 resolution, opens a borderless fullscreen overlay where
//! the user drags a rectangle, and provides "Send to Chat", "Copy", "Save",
//! "Fullscreen", and "Cancel" (`Esc` / `Enter`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    div, hsla, img, point, prelude::*, px, rgb, size, App, Bounds, ClickEvent, Context,
    CursorStyle, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Render, RenderImage,
    StatefulInteractiveElement, Styled, Window, WindowBounds, WindowDecorations, WindowOptions,
};
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, ImageFormat, MapState};
use x11rb::rust_connection::RustConnection;

use crate::fonts::FAMILY_SEMIBOLD;

/// Outcome emitted when the user finishes or cancels the screenshot overlay.
#[derive(Clone, Debug)]
pub(crate) enum ShotOutcome {
    /// User chose "Send to Chat": PNG saved at `path` and raw `png_bytes` ready for clipboard.
    Send { path: PathBuf, png_bytes: Vec<u8> },
    /// User chose "Copy": raw `png_bytes` copied to clipboard only.
    Copied { png_bytes: Vec<u8> },
    /// User chose "Save": PNG saved permanently under `~/Pictures/`.
    Saved { path: PathBuf, png_bytes: Vec<u8> },
    /// User pressed `Esc` or clicked "Cancel".
    Cancelled,
}

/// Raw 1:1 monitor screenshot grabbed natively via X11 `GetImage`.
pub(crate) struct CapturedMonitor {
    /// Monitor left X in X11 virtual root coordinates.
    pub x: i16,
    /// Monitor top Y in X11 virtual root coordinates.
    pub y: i16,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tightly packed BGRA `[B, G, R, 255]` (`width * height * 4` bytes).
    pub bgra: Vec<u8>,
    /// Visible top-level windows in monitor-local pixels `(l, t, r, b)`, topmost first.
    pub windows: Vec<(f32, f32, f32, f32)>,
}

/// Returns `true` when native X11 `GetImage` screenshot capture is available.
#[must_use]
pub(crate) fn native_x11_available() -> bool {
    std::env::var_os("DISPLAY").is_some() && RustConnection::connect(None).is_ok()
}

/// Captures the active physical monitor (containing the pointer, or primary)
/// at full 1:1 native resolution via `x11rb` + RandR.
///
/// # Errors
///
/// Returns an error if X11 cannot be contacted or `GetImage` fails.
pub(crate) fn grab_active_monitor() -> Result<CapturedMonitor, String> {
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| format!("Không kết nối được X11: {e}"))?;
    let screen = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or_else(|| "Không tìm thấy màn hình X11.".to_owned())?;
    let root = screen.root;
    let sw = screen.width_in_pixels;
    let sh = screen.height_in_pixels;

    let pointer = conn
        .query_pointer(root)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| (r.root_x, r.root_y));

    let (mx, my, mw, mh) = pick_monitor_rect(&conn, root, sw, sh, pointer);
    let windows = collect_visible_windows(&conn, root, mx, my, mw, mh);

    let reply = conn
        .get_image(ImageFormat::Z_PIXMAP, root, mx, my, mw, mh, !0)
        .map_err(|e| format!("Không chụp được màn hình X11: {e}"))?
        .reply()
        .map_err(|e| format!("Màn hình X11 không trả ảnh: {e}"))?;

    let w = usize::from(mw);
    let h = usize::from(mh);
    let bpp = reply.data.len() / (w * h).max(1);
    if bpp < 3 || reply.data.len() < w * h * bpp {
        return Err("Định dạng pixel X11 không hợp lệ.".into());
    }
    let mut bgra = vec![255_u8; w * h * 4];
    if bpp == 4 {
        bgra.copy_from_slice(&reply.data[..w * h * 4]);
        for px in bgra.chunks_exact_mut(4) {
            px[3] = 255;
        }
    } else {
        for i in 0..(w * h) {
            let s = i * bpp;
            let d = i * 4;
            bgra[d] = reply.data[s];
            bgra[d + 1] = reply.data[s + 1];
            bgra[d + 2] = reply.data[s + 2];
            bgra[d + 3] = 255;
        }
    }

    Ok(CapturedMonitor {
        x: mx,
        y: my,
        width: u32::from(mw),
        height: u32::from(mh),
        bgra,
        windows,
    })
}

/// Collect visible top-level window bounds in monitor-local coordinates (topmost first)
/// so hovering a window in the screenshot overlay highlights it for 1-click capture.
fn collect_visible_windows(
    conn: &RustConnection,
    root: u32,
    mx: i16,
    my: i16,
    mw: u16,
    mh: u16,
) -> Vec<(f32, f32, f32, f32)> {
    let intern = |name: &[u8]| -> Option<u32> {
        conn.intern_atom(false, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
            .filter(|a| *a != 0)
    };
    let Some(stacking_atom) = intern(b"_NET_CLIENT_LIST_STACKING") else {
        return Vec::new();
    };
    let wm_type_atom = intern(b"_NET_WM_WINDOW_TYPE").unwrap_or(0);
    let type_desktop = intern(b"_NET_WM_WINDOW_TYPE_DESKTOP").unwrap_or(0);
    let type_dock = intern(b"_NET_WM_WINDOW_TYPE_DOCK").unwrap_or(0);
    let type_notif = intern(b"_NET_WM_WINDOW_TYPE_NOTIFICATION").unwrap_or(0);
    let wm_state_atom = intern(b"_NET_WM_STATE").unwrap_or(0);
    let state_hidden = intern(b"_NET_WM_STATE_HIDDEN").unwrap_or(0);
    let frame_extents_atom = intern(b"_NET_FRAME_EXTENTS").unwrap_or(0);

    let list: Vec<u32> = conn
        .get_property(false, root, stacking_atom, AtomEnum::WINDOW, 0, 256)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().map(|it| it.collect()))
        .unwrap_or_default();

    let mx_f = f32::from(mx);
    let my_f = f32::from(my);
    let mw_f = f32::from(mw);
    let mh_f = f32::from(mh);
    let mut out = Vec::new();

    // `_NET_CLIENT_LIST_STACKING` is bottom-to-top; iterate in reverse so topmost wins.
    for win in list.into_iter().rev() {
        let Some(attrs) = conn
            .get_window_attributes(win)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            continue;
        };
        if attrs.map_state != MapState::VIEWABLE {
            continue;
        }
        if wm_state_atom != 0 && state_hidden != 0 {
            let hidden = conn
                .get_property(false, win, wm_state_atom, AtomEnum::ATOM, 0, 32)
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| r.value32().map(|mut it| it.any(|a| a == state_hidden)))
                .unwrap_or(false);
            if hidden {
                continue;
            }
        }
        if wm_type_atom != 0 {
            let skip = conn
                .get_property(false, win, wm_type_atom, AtomEnum::ATOM, 0, 16)
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| {
                    r.value32().map(|mut it| {
                        it.any(|a| {
                            (type_desktop != 0 && a == type_desktop)
                                || (type_dock != 0 && a == type_dock)
                                || (type_notif != 0 && a == type_notif)
                        })
                    })
                })
                .unwrap_or(false);
            if skip {
                continue;
            }
        }
        let Some(geo) = conn.get_geometry(win).ok().and_then(|c| c.reply().ok()) else {
            continue;
        };
        let Some(tr) = conn
            .translate_coordinates(win, root, 0, 0)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            continue;
        };
        let (fl, fr, ft, fb) = if frame_extents_atom != 0 {
            conn.get_property(false, win, frame_extents_atom, AtomEnum::CARDINAL, 0, 4)
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| r.value32().map(|it| it.collect::<Vec<_>>()))
                .filter(|v| v.len() == 4)
                .map(|v| (v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32))
                .unwrap_or((0.0, 0.0, 0.0, 0.0))
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };
        let l = (f32::from(tr.dst_x) - fl - mx_f).clamp(0.0, mw_f);
        let t = (f32::from(tr.dst_y) - ft - my_f).clamp(0.0, mh_f);
        let r = (f32::from(tr.dst_x) + f32::from(geo.width) + fr - mx_f).clamp(0.0, mw_f);
        let b = (f32::from(tr.dst_y) + f32::from(geo.height) + fb - my_f).clamp(0.0, mh_f);
        if r - l >= 48.0 && b - t >= 48.0 {
            out.push((l, t, r, b));
        }
    }
    out
}

fn window_at_point(
    windows: &[(f32, f32, f32, f32)],
    px_x: f32,
    px_y: f32,
) -> Option<(f32, f32, f32, f32)> {
    windows
        .iter()
        .copied()
        .find(|&(l, t, r, b)| px_x >= l && px_x <= r && px_y >= t && px_y <= b)
}

fn pick_monitor_rect(
    conn: &RustConnection,
    root: u32,
    sw: u16,
    sh: u16,
    pointer: Option<(i16, i16)>,
) -> (i16, i16, u16, u16) {
    let monitors: Vec<(i16, i16, u16, u16, bool)> = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| {
            r.monitors
                .into_iter()
                .filter(|m| m.width >= 320 && m.height >= 200)
                .map(|m| (m.x, m.y, m.width, m.height, m.primary))
                .collect()
        })
        .unwrap_or_default();

    if let Some((px_x, px_y)) = pointer {
        if let Some(&(x, y, w, h, _)) = monitors.iter().find(|&&(x, y, w, h, _)| {
            i32::from(px_x) >= i32::from(x)
                && i32::from(px_x) < i32::from(x) + i32::from(w)
                && i32::from(px_y) >= i32::from(y)
                && i32::from(px_y) < i32::from(y) + i32::from(h)
        }) {
            return (x, y, w, h);
        }
    }
    if let Some(&(x, y, w, h, _)) = monitors.iter().find(|m| m.4).or_else(|| monitors.first()) {
        return (x, y, w, h);
    }
    (0, 0, sw, sh)
}

/// Crops `rect` (`x, y, w, h` in pixel coordinates) from `bgra` (`src_w × src_h`)
/// and encodes a crisp PNG buffer.
pub(crate) fn crop_bgra_to_png(
    bgra: &[u8],
    src_w: u32,
    src_h: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<Vec<u8>, String> {
    let x0 = x.min(src_w.saturating_sub(1));
    let y0 = y.min(src_h.saturating_sub(1));
    let cw = w.min(src_w.saturating_sub(x0)).max(1);
    let ch = h.min(src_h.saturating_sub(y0)).max(1);

    let mut rgba = vec![255_u8; (cw as usize) * (ch as usize) * 4];
    let src_stride = (src_w as usize) * 4;
    for row in 0..(ch as usize) {
        let s_row = ((y0 as usize) + row) * src_stride + (x0 as usize) * 4;
        let d_row = row * (cw as usize) * 4;
        for col in 0..(cw as usize) {
            let s = s_row + col * 4;
            let d = d_row + col * 4;
            rgba[d] = bgra[s + 2];
            rgba[d + 1] = bgra[s + 1];
            rgba[d + 2] = bgra[s];
            rgba[d + 3] = 255;
        }
    }
    let img_buf = image::RgbaImage::from_raw(cw, ch, rgba)
        .ok_or_else(|| "Không tạo được ảnh cắt.".to_owned())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img_buf)
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| format!("Không mã hóa được PNG: {e}"))?;
    Ok(out.into_inner())
}

fn temp_shot_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("zalo-gpui-shot-{nanos}.png"))
}

fn saved_shot_path() -> PathBuf {
    let base = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|h| h.join("Pictures"))
        .filter(|p| p.is_dir() || std::fs::create_dir_all(p).is_ok())
        .unwrap_or_else(std::env::temp_dir);
    let stamp = crate::calendar::local_stamp_dmy_hms();
    base.join(format!("ZaloScreenshot_{stamp}.png"))
}

/// Opens the borderless fullscreen region-crop overlay on the captured monitor.
pub(crate) fn open_screenshot_overlay(
    capture: CapturedMonitor,
    on_done: impl FnOnce(ShotOutcome, &mut App) + 'static,
    cx: &mut App,
) -> Result<(), String> {
    let rgba_frame = image::RgbaImage::from_raw(capture.width, capture.height, capture.bgra.clone())
        .ok_or_else(|| "Không dựng được khung ảnh chụp màn hình.".to_owned())?;
    let render_img = Arc::new(RenderImage::new([image::Frame::new(rgba_frame)]));

    let bounds = Bounds::new(
        point(px(f32::from(capture.x)), px(f32::from(capture.y))),
        size(px(capture.width as f32), px(capture.height as f32)),
    );
    let callback = std::cell::RefCell::new(Some(Box::new(on_done)
        as Box<dyn FnOnce(ShotOutcome, &mut App)>));

    crate::x11_frame::anticipate_csd_window();
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Fullscreen(bounds)),
            titlebar: None,
            focus: true,
            show: true,
            is_resizable: false,
            is_movable: false,
            is_minimizable: false,
            display_id: None,
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some("zalo-gpui-shot".into()),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title("Zalo Screenshot");
            let view = cx.new(|cx| {
                cx.on_release(|this: &mut ShotOverlayView, cx: &mut App| {
                    if let Some(cb) = this.on_done.borrow_mut().take() {
                        cb(ShotOutcome::Cancelled, cx);
                    }
                })
                .detach();
                ShotOverlayView {
                    capture,
                    image: render_img,
                    focus: cx.focus_handle(),
                    dragging: false,
                    start: None,
                    end: None,
                    hover_win: None,
                    on_done: callback,
                }
            });
            let focus = view.read(cx).focus.clone();
            window.focus(&focus);
            view
        },
    )
    .map_err(|e| format!("Không mở được lớp chọn vùng chụp: {e}"))?;
    Ok(())
}

struct ShotOverlayView {
    capture: CapturedMonitor,
    image: Arc<RenderImage>,
    focus: FocusHandle,
    dragging: bool,
    start: Option<(f32, f32)>,
    end: Option<(f32, f32)>,
    /// Auto-detected window under cursor (viewport coordinates `(l, t, r, b)`).
    hover_win: Option<(f32, f32, f32, f32)>,
    on_done: std::cell::RefCell<Option<Box<dyn FnOnce(ShotOutcome, &mut App)>>>,
}

impl Focusable for ShotOverlayView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ShotOverlayView {
    fn selection_rect(&self, vp_w: f32, vp_h: f32) -> Option<(f32, f32, f32, f32)> {
        let (x0, y0) = self.start?;
        let (x1, y1) = self.end?;
        let l = x0.min(x1).clamp(0.0, vp_w);
        let t = y0.min(y1).clamp(0.0, vp_h);
        let r = x0.max(x1).clamp(0.0, vp_w);
        let b = y0.max(y1).clamp(0.0, vp_h);
        if r - l >= 6.0 && b - t >= 6.0 {
            Some((l, t, r, b))
        } else {
            None
        }
    }

    fn finish(&mut self, outcome: ShotOutcome, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(cb) = self.on_done.borrow_mut().take() {
            cx.defer(move |cx| cb(outcome, cx));
        }
        window.remove_window();
    }

    fn crop_current(&self, window: &Window) -> Result<Vec<u8>, String> {
        let vp = window.viewport_size();
        let vp_w = f32::from(vp.width).max(1.0);
        let vp_h = f32::from(vp.height).max(1.0);
        let sx = self.capture.width as f32 / vp_w;
        let sy = self.capture.height as f32 / vp_h;
        let (l, t, r, b) = self
            .selection_rect(vp_w, vp_h)
            .unwrap_or((0.0, 0.0, vp_w, vp_h));
        let px_x = (l * sx).round().max(0.0) as u32;
        let px_y = (t * sy).round().max(0.0) as u32;
        let px_w = ((r - l) * sx).round().max(1.0) as u32;
        let px_h = ((b - t) * sy).round().max(1.0) as u32;
        crop_bgra_to_png(
            &self.capture.bgra,
            self.capture.width,
            self.capture.height,
            px_x,
            px_y,
            px_w,
            px_h,
        )
    }

    fn commit_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(png_bytes) = self.crop_current(window) else {
            self.finish(ShotOutcome::Cancelled, window, cx);
            return;
        };
        let path = temp_shot_path();
        if std::fs::write(&path, &png_bytes).is_ok() {
            self.finish(ShotOutcome::Send { path, png_bytes }, window, cx);
        } else {
            self.finish(ShotOutcome::Cancelled, window, cx);
        }
    }

    fn commit_copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(png_bytes) = self.crop_current(window) {
            self.finish(ShotOutcome::Copied { png_bytes }, window, cx);
        } else {
            self.finish(ShotOutcome::Cancelled, window, cx);
        }
    }

    fn commit_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(png_bytes) = self.crop_current(window) else {
            self.finish(ShotOutcome::Cancelled, window, cx);
            return;
        };
        let path = saved_shot_path();
        if std::fs::write(&path, &png_bytes).is_ok() {
            self.finish(ShotOutcome::Saved { path, png_bytes }, window, cx);
        } else {
            self.finish(ShotOutcome::Cancelled, window, cx);
        }
    }
}

impl Render for ShotOverlayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vp = window.viewport_size();
        let vp_w = f32::from(vp.width).max(1.0);
        let vp_h = f32::from(vp.height).max(1.0);
        let sel = self.selection_rect(vp_w, vp_h);
        let active_rect = sel.or(self.hover_win);
        let has_locked = sel.is_some();
        let scrim = hsla(0., 0., 0., 0.46);

        let mut root = div()
            .id("zalo-shot-overlay")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                let ctrl = e.keystroke.modifiers.control;
                match e.keystroke.key.as_str() {
                    "escape" => this.finish(ShotOutcome::Cancelled, window, cx),
                    "enter" => this.commit_send(window, cx),
                    "c" | "C" if ctrl => this.commit_copy(window, cx),
                    "s" | "S" if ctrl => this.commit_save(window, cx),
                    _ => {}
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    if this.start.is_some() || this.end.is_some() {
                        this.dragging = false;
                        this.start = None;
                        this.end = None;
                        cx.notify();
                    } else {
                        this.finish(ShotOutcome::Cancelled, window, cx);
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                if this.dragging {
                    if e.pressed_button != Some(MouseButton::Left) {
                        this.dragging = false;
                        cx.notify();
                        return;
                    }
                    this.end = Some((f32::from(e.position.x), f32::from(e.position.y)));
                    cx.notify();
                    return;
                }
                let vp = window.viewport_size();
                let vw = f32::from(vp.width).max(1.0);
                let vh = f32::from(vp.height).max(1.0);
                if this.selection_rect(vw, vh).is_none() {
                    let sx = this.capture.width as f32 / vw;
                    let sy = this.capture.height as f32 / vh;
                    let next = window_at_point(
                        &this.capture.windows,
                        f32::from(e.position.x) * sx,
                        f32::from(e.position.y) * sy,
                    )
                    .map(|(l, t, r, b)| (l / sx, t / sy, r / sx, b / sy));
                    if this.hover_win != next {
                        this.hover_win = next;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, window, cx| {
                    if this.dragging {
                        let ex = f32::from(e.position.x);
                        let ey = f32::from(e.position.y);
                        let small_click = this
                            .start
                            .is_some_and(|(sx, sy)| (ex - sx).abs() < 6.0 && (ey - sy).abs() < 6.0);
                        this.dragging = false;
                        if small_click {
                            // Single click without dragging locks the detected window under cursor.
                            let vp = window.viewport_size();
                            let vw = f32::from(vp.width).max(1.0);
                            let vh = f32::from(vp.height).max(1.0);
                            let sx = this.capture.width as f32 / vw;
                            let sy = this.capture.height as f32 / vh;
                            if let Some((l, t, r, b)) = window_at_point(
                                &this.capture.windows,
                                ex * sx,
                                ey * sy,
                            ) {
                                this.start = Some((l / sx, t / sy));
                                this.end = Some((r / sx, b / sy));
                            } else {
                                this.start = None;
                                this.end = None;
                            }
                        } else {
                            this.end = Some((ex, ey));
                        }
                        cx.notify();
                    }
                }),
            )
            // Full-screen frozen desktop bitmap
            .child(
                img(Arc::clone(&self.image))
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(gpui::ObjectFit::Fill),
            );

        if let Some((l, t, r, b)) = active_rect {
            let sw = r - l;
            let sh = b - t;
            let sx = self.capture.width as f32 / vp_w;
            let sy = self.capture.height as f32 / vp_h;
            let dim_label = format!(
                "{} × {}",
                (sw * sx).round() as u32,
                (sh * sy).round() as u32
            );
            let toolbar_w = 344.0_f32;
            let toolbar_h = 38.0_f32;
            let bar_x = (r - toolbar_w).clamp(12.0, (vp_w - toolbar_w - 12.0).max(12.0));
            let bar_y = if b + toolbar_h + 12.0 <= vp_h {
                b + 8.0
            } else if t >= toolbar_h + 12.0 {
                t - toolbar_h - 8.0
            } else {
                (b - toolbar_h - 8.0).max(12.0)
            };
            let badge_y = if t >= 28.0 { t - 26.0 } else { t + 6.0 };

            // 4 dim panels around the bright selected/hovered rectangle
            root = root
                .child(div().absolute().left_0().top_0().w_full().h(px(t)).bg(scrim))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(b))
                        .w_full()
                        .h(px((vp_h - b).max(0.0)))
                        .bg(scrim),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(t))
                        .w(px(l))
                        .h(px(sh))
                        .bg(scrim),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(r))
                        .top(px(t))
                        .w(px((vp_w - r).max(0.0)))
                        .h(px(sh))
                        .bg(scrim),
                )
                // Selection / hover border
                .child(
                    div()
                        .absolute()
                        .left(px(l))
                        .top(px(t))
                        .w(px(sw))
                        .h(px(sh))
                        .border(px(2.))
                        .border_color(rgb(0x00_68_ff)),
                )
                // Pixel dimensions badge
                .child(
                    div()
                        .absolute()
                        .left(px(l.clamp(8.0, (vp_w - 110.0).max(8.0))))
                        .top(px(badge_y))
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(4.))
                        .bg(hsla(0., 0., 0., 0.78))
                        .text_size(px(11.5))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(0xff_ff_ff))
                        .child(dim_label),
                );

            // Fullscreen drag-start / double-click surface rendered BEFORE the toolbar
            root = root.child(
                div()
                    .id("zalo-shot-canvas")
                    .absolute()
                    .inset_0()
                    .cursor(CursorStyle::Crosshair)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                            let px_x = f32::from(e.position.x);
                            let px_y = f32::from(e.position.y);
                            if e.click_count >= 2 && has_locked && px_x >= l && px_x <= r && px_y >= t && px_y <= b {
                                this.commit_send(window, cx);
                                return;
                            }
                            let p = (px_x, px_y);
                            this.start = Some(p);
                            this.end = Some(p);
                            this.dragging = true;
                            cx.notify();
                        }),
                    ),
            );

            if has_locked && !self.dragging {
                root = root.child(
                    div()
                        .id("zalo-shot-toolbar")
                        .absolute()
                        .left(px(bar_x))
                        .top(px(bar_y))
                        .h(px(toolbar_h))
                        .px(px(8.))
                        .rounded(px(8.))
                        .bg(rgb(0x1f_24_2d))
                        .border_1()
                        .border_color(hsla(0., 0., 1., 0.16))
                        .shadow_lg()
                        .cursor(CursorStyle::Arrow)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .occlude()
                        .child(
                            div()
                                .id("shot-btn-cancel")
                                .h(px(26.))
                                .px(px(9.))
                                .rounded(px(5.))
                                .bg(hsla(0., 0., 1., 0.08))
                                .hover(|s| s.bg(hsla(0., 0., 1., 0.16)))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .text_size(px(12.))
                                .text_color(rgb(0xff_ff_ff))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.finish(ShotOutcome::Cancelled, window, cx);
                                }))
                                .child("✕ Hủy"),
                        )
                        .child(
                            div()
                                .id("shot-btn-save")
                                .h(px(26.))
                                .px(px(9.))
                                .rounded(px(5.))
                                .bg(hsla(0., 0., 1., 0.08))
                                .hover(|s| s.bg(hsla(0., 0., 1., 0.16)))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .text_size(px(12.))
                                .text_color(rgb(0xff_ff_ff))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.commit_save(window, cx);
                                }))
                                .child("Lưu"),
                        )
                        .child(
                            div()
                                .id("shot-btn-copy")
                                .h(px(26.))
                                .px(px(9.))
                                .rounded(px(5.))
                                .bg(hsla(0., 0., 1., 0.08))
                                .hover(|s| s.bg(hsla(0., 0., 1., 0.16)))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .text_size(px(12.))
                                .text_color(rgb(0xff_ff_ff))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.commit_copy(window, cx);
                                }))
                                .child("Sao chép"),
                        )
                        .child(
                            div()
                                .id("shot-btn-send")
                                .h(px(26.))
                                .px(px(12.))
                                .rounded(px(5.))
                                .bg(rgb(0x00_68_ff))
                                .hover(|s| s.bg(rgb(0x00_5a_e0)))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .text_size(px(12.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(0xff_ff_ff))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.commit_send(window, cx);
                                }))
                                .child("✓ Gửi vào chat"),
                        ),
                );
            }
        } else {
            // Dim full screen + drag surface, with centered floating hint bar on top
            root = root
                .child(div().absolute().inset_0().bg(scrim))
                .child(
                    div()
                        .id("zalo-shot-canvas")
                        .absolute()
                        .inset_0()
                        .cursor(CursorStyle::Crosshair)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                let p = (f32::from(e.position.x), f32::from(e.position.y));
                                this.start = Some(p);
                                this.end = Some(p);
                                this.dragging = true;
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(22.))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .px(px(16.))
                                .py(px(8.))
                                .rounded(px(20.))
                                .bg(hsla(0., 0., 0., 0.82))
                                .border_1()
                                .border_color(hsla(0., 0., 1., 0.18))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(12.))
                                .cursor(CursorStyle::Arrow)
                                .occlude()
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .text_color(rgb(0xff_ff_ff))
                                        .child("Bấm chọn cửa sổ hoặc kéo chọn vùng • Enter chụp toàn màn hình • Esc hủy"),
                                )
                                .child(
                                    div()
                                        .id("shot-full-now")
                                        .px(px(10.))
                                        .py(px(3.))
                                        .rounded(px(12.))
                                        .bg(rgb(0x00_68_ff))
                                        .hover(|s| s.bg(rgb(0x00_5a_e0)))
                                        .cursor_pointer()
                                        .text_size(px(12.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(0xff_ff_ff))
                                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                            this.commit_send(window, cx);
                                        }))
                                        .child("Toàn màn hình"),
                                )
                                .child(
                                    div()
                                        .id("shot-cancel-top")
                                        .px(px(8.))
                                        .py(px(3.))
                                        .rounded(px(12.))
                                        .bg(hsla(0., 0., 1., 0.14))
                                        .hover(|s| s.bg(hsla(0., 0., 1., 0.24)))
                                        .cursor_pointer()
                                        .text_size(px(12.))
                                        .text_color(rgb(0xff_ff_ff))
                                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                            this.finish(ShotOutcome::Cancelled, window, cx);
                                        }))
                                        .child("Hủy"),
                                ),
                        ),
                );
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_bgra_region_into_valid_png_with_expected_rgb() {
        // 4×4 solid orange in BGRA = [B=10, G=120, R=240, A=255]
        let mut bgra = vec![0_u8; 4 * 4 * 4];
        for px in bgra.chunks_exact_mut(4) {
            px.copy_from_slice(&[10, 120, 240, 255]);
        }
        let png = crop_bgra_to_png(&bgra, 4, 4, 1, 1, 2, 2).expect("crop png");
        let decoded = image::load_from_memory(&png).expect("decode png").to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 2));
        let p = decoded.get_pixel(0, 0).0;
        assert_eq!(p, [240, 120, 10, 255]);
    }

    #[test]
    fn window_at_point_returns_topmost_matching_window() {
        let windows = vec![
            (100.0, 100.0, 400.0, 300.0), // topmost
            (0.0, 0.0, 1000.0, 700.0),    // underneath
        ];
        assert_eq!(window_at_point(&windows, 200.0, 200.0), Some(windows[0]));
        assert_eq!(window_at_point(&windows, 50.0, 50.0), Some(windows[1]));
        assert_eq!(window_at_point(&windows, 1200.0, 200.0), None);
    }
}
