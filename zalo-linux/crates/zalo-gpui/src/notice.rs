//! Screen-corner message popup, separate from the main Zalo window.
//!
//! Official Zalo (`znotification`) is a frameless always-on-top window,
//! 352×82, parked on the primary display work area with a 5 px margin.
//! The four corners here are that work area, plus a dragged custom spot.
//! They are not the corners of the Zalo window. `xfce4-notifyd` ignores
//! x/y, so this window is positioned by us.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

use gpui::{
    div, point, prelude::*, px, rgb, size, App, Bounds, Context, IntoElement, ParentElement,
    Render, RenderImage, Styled, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowHandle, WindowKind, WindowOptions,
};
use std::sync::Arc;

use crate::color::token_rgb;

/// Outer window, matching Electron `znotification` (`352×82`).
pub const NOTICE_W: f32 = 352.0;
/// Outer window height.
pub const NOTICE_H: f32 = 82.0;
/// Gap between the card and the work-area edge. Official `g = 5`.
const MARGIN: f32 = 5.0;
/// `#zbg` radius.
const CARD_RADIUS: f32 = 4.0;

/// One popup: title, one-line body, avatar initial (and a photo when we have one).
pub struct NoticeContent {
    /// Conversation name, or the sample title.
    pub title: String,
    /// Message line. Newlines already turned into spaces.
    pub body: String,
    /// Letter drawn when no avatar bitmap is cached.
    pub initial: String,
    /// Cached avatar, when the shell already decoded one.
    pub image: Option<Arc<RenderImage>>,
    /// Thread whose photo should replace the letter while this popup is open.
    pub thread_key: String,
}

/// Electron `znotification` replaces CR/LF with a space before the card clips.
#[must_use]
pub fn notice_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

struct Target {
    x: f32,
    y: f32,
    lock: bool,
}

fn target_slot() -> &'static Mutex<Option<Target>> {
    static TARGET: OnceLock<Mutex<Option<Target>>> = OnceLock::new();
    TARGET.get_or_init(|| Mutex::new(None))
}

fn open_slot() -> &'static Mutex<Option<WindowHandle<NoticeView>>> {
    static OPEN: OnceLock<Mutex<Option<WindowHandle<NoticeView>>>> = OnceLock::new();
    OPEN.get_or_init(|| Mutex::new(None))
}

fn custom_slot() -> &'static Mutex<Option<(f32, f32)>> {
    static CUSTOM: OnceLock<Mutex<Option<(f32, f32)>>> = OnceLock::new();
    CUSTOM.get_or_init(|| Mutex::new(None))
}

/// Screen origin the X11 helper should snap a locked notice to.
/// `None` while the user is dragging.
pub(crate) fn locked_origin() -> Option<(f32, f32)> {
    let guard = target_slot().lock().ok()?;
    let target = guard.as_ref()?;
    target.lock.then_some((target.x, target.y))
}

fn set_target(x: f32, y: f32, lock: bool) {
    if let Ok(mut guard) = target_slot().lock() {
        *guard = Some(Target { x, y, lock });
    }
}

fn unlock() {
    if let Ok(mut guard) = target_slot().lock() {
        if let Some(target) = guard.as_mut() {
            target.lock = false;
        }
    }
}

/// Dropped screen position from a finished drag. The shell writes it into prefs.
pub(crate) fn take_custom() -> Option<(f32, f32)> {
    custom_slot().lock().ok().and_then(|mut guard| guard.take())
}

fn remember_custom(x: f32, y: f32, cx: &mut App) {
    if let Ok(mut guard) = custom_slot().lock() {
        *guard = Some((x, y));
    }
    set_target(x, y, false);
    crate::shell::poke_notice_wake(cx);
}

/// Top-left of the 352×82 window inside a work area `(x, y, w, h)`.
///
/// `tl` / `tr` / `bl` / `br` sit 5 px in from that rectangle. `custom` uses
/// the saved screen origin, clamped so the window stays inside the area.
#[must_use]
pub fn screen_origin(
    corner: &str,
    saved_x: f32,
    saved_y: f32,
    work_x: f32,
    work_y: f32,
    work_w: f32,
    work_h: f32,
) -> (f32, f32) {
    let left = work_x + MARGIN;
    let top = work_y + MARGIN;
    let right = work_x + work_w - NOTICE_W - MARGIN;
    let bottom = work_y + work_h - NOTICE_H - MARGIN;
    let right = right.max(left);
    let bottom = bottom.max(top);
    match corner {
        "tl" => (left, top),
        "bl" => (left, bottom),
        "br" => (right, bottom),
        "custom" if saved_x != 0.0 || saved_y != 0.0 => (
            saved_x.clamp(work_x, right + MARGIN),
            saved_y.clamp(work_y, bottom + MARGIN),
        ),
        _ => (right, top),
    }
}

/// `_NET_WORKAREA` in logical pixels, else `fallback` (full display bounds).
pub fn work_area(scale: f32, fallback: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let Some((x, y, w, h)) = net_work_area() else {
        return fallback;
    };
    if w <= 0 || h <= 0 {
        return fallback;
    }
    let s = scale.max(0.5);
    (x as f32 / s, y as f32 / s, w as f32 / s, h as f32 / s)
}

fn net_work_area() -> Option<(i32, i32, i32, i32)> {
    if cfg!(test) || std::env::var_os("DISPLAY").is_none() {
        return None;
    }
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let atom = conn
        .intern_atom(false, b"_NET_WORKAREA")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let reply = conn
        .get_property(false, root, atom, 0u32, 0, 4)
        .ok()?
        .reply()
        .ok()?;
    let values: Vec<u32> = reply.value32()?.collect();
    if values.len() < 4 {
        return None;
    }
    Some((
        values[0] as i32,
        values[1] as i32,
        values[2] as i32,
        values[3] as i32,
    ))
}

fn area_for(cx: &App, scale: f32) -> (f32, f32, f32, f32) {
    let fallback = cx
        .primary_display()
        .map(|display| {
            let bounds = display.bounds();
            (
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            )
        })
        .unwrap_or((0.0, 0.0, 1920.0, 1080.0));
    work_area(scale, fallback)
}

/// Open or reuse the popup at `corner` (`tl`/`tr`/`bl`/`br`/`custom`).
pub fn present(
    cx: &mut App,
    scale: f32,
    content: NoticeContent,
    corner: &str,
    saved_x: f32,
    saved_y: f32,
) {
    let (wx, wy, ww, wh) = area_for(cx, scale);
    let (x, y) = screen_origin(corner, saved_x, saved_y, wx, wy, ww, wh);
    set_target(x, y, true);
    if let Some(handle) = open_slot().lock().ok().and_then(|guard| *guard) {
        if handle.read(cx).is_ok() {
            let _ = handle.update(cx, |view, _, cx| {
                view.replace(content, cx);
                cx.notify();
            });
            return;
        }
        if let Ok(mut guard) = open_slot().lock() {
            *guard = None;
        }
    }
    // Stamp the corner before GPUI maps, so xfwm does not center the card.
    crate::x11_frame::anticipate_notice(scale);
    let bounds = Bounds {
        origin: point(px(x), px(y)),
        size: size(px(NOTICE_W), px(NOTICE_H)),
    };
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::Normal,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            window_decorations: Some(WindowDecorations::Client),
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("zalo-gpui".into()),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title("Zalo Notice");
            // Map already happened inside GPUI. Withdraw and place before the
            // first draw so xfwm does not show the card in the screen center.
            crate::x11_frame::park_notice(window.scale_factor());
            window.set_client_inset(px(0.));
            window.set_background_appearance(WindowBackgroundAppearance::Transparent);
            cx.new(|cx| NoticeView::new(content, cx))
        },
    );
    if let Ok(handle) = opened {
        if let Ok(mut guard) = open_slot().lock() {
            *guard = Some(handle);
        }
    }
}

/// Put a decoded avatar on the open popup when it is still this thread and
/// still showing the letter. Does not restart the dismiss timer.
pub fn patch_avatar(cx: &mut App, thread_key: &str, image: Arc<RenderImage>) -> bool {
    let Some(handle) = open_slot().lock().ok().and_then(|guard| *guard) else {
        return false;
    };
    if handle.read(cx).is_err() {
        if let Ok(mut guard) = open_slot().lock() {
            *guard = None;
        }
        return false;
    }
    let key = thread_key.to_owned();
    handle
        .update(cx, |view, _, cx| {
            if view.expired || view.content.thread_key != key || view.content.image.is_some() {
                return false;
            }
            view.content.image = Some(image);
            cx.notify();
            true
        })
        .unwrap_or(false)
}

/// Move an open popup when the settings corner changes. No window is created.
pub fn move_if_open(cx: &mut App, scale: f32, corner: &str, saved_x: f32, saved_y: f32) {
    let Some(handle) = open_slot().lock().ok().and_then(|guard| *guard) else {
        return;
    };
    if handle.read(cx).is_err() {
        if let Ok(mut guard) = open_slot().lock() {
            *guard = None;
        }
        return;
    }
    let (wx, wy, ww, wh) = area_for(cx, scale);
    let (x, y) = screen_origin(corner, saved_x, saved_y, wx, wy, ww, wh);
    set_target(x, y, true);
}

struct NoticeView {
    content: NoticeContent,
    gen: u64,
    expired: bool,
    drag_at: Option<Instant>,
    last_origin: (f32, f32),
    moved: bool,
    still_since: Instant,
}

impl NoticeView {
    fn new(content: NoticeContent, cx: &mut Context<Self>) -> Self {
        let view = Self {
            content,
            gen: 1,
            expired: false,
            drag_at: None,
            last_origin: (0.0, 0.0),
            moved: false,
            still_since: Instant::now(),
        };
        view.arm(1, cx);
        Self::arm_snap(cx);
        view
    }

    fn replace(&mut self, content: NoticeContent, cx: &mut Context<Self>) {
        self.content = content;
        self.gen = self.gen.wrapping_add(1);
        let gen = self.gen;
        self.arm(gen, cx);
        Self::arm_snap(cx);
    }

    /// Keep rendering briefly so the X11 snap can run after xfwm places us.
    ///
    /// An idle popup paints once. That one paint used to land inside the
    /// frame-shape throttle, and the card stayed in the middle of the screen.
    fn arm_snap(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            for _ in 0..8 {
                gpui::Timer::after(Duration::from_millis(50)).await;
                let keep = this.update(cx, |view, cx| {
                    if view.expired {
                        return false;
                    }
                    cx.notify();
                    true
                });
                if !keep.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    fn arm(&self, gen: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            gpui::Timer::after(Duration::from_secs(5)).await;
            let _ = this.update(cx, |view, cx| {
                if view.gen != gen {
                    return;
                }
                if view.drag_at.is_some() {
                    view.arm(gen, cx);
                    return;
                }
                view.expired = true;
                cx.notify();
            });
        })
        .detach();
    }

    fn dismiss(&mut self, window: &mut Window) {
        if let Ok(mut guard) = open_slot().lock() {
            *guard = None;
        }
        unlock();
        window.remove_window();
    }

    fn watch_drag(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(started) = self.drag_at else {
            return;
        };
        let origin = window.bounds().origin;
        let here = (f32::from(origin.x), f32::from(origin.y));
        let moved =
            (here.0 - self.last_origin.0).abs() > 2.0 || (here.1 - self.last_origin.1).abs() > 2.0;
        if moved {
            self.last_origin = here;
            self.moved = true;
            self.still_since = Instant::now();
            cx.spawn(async move |this, cx| {
                gpui::Timer::after(Duration::from_millis(200)).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
            return;
        }
        if self.moved && self.still_since.elapsed() > Duration::from_millis(150) {
            remember_custom(here.0, here.1, cx);
            self.drag_at = None;
            self.moved = false;
            return;
        }
        if !self.moved && started.elapsed() > Duration::from_millis(400) {
            self.drag_at = None;
        }
    }
}

impl Render for NoticeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::x11_frame::sync(window.scale_factor());
        self.watch_drag(window, cx);
        if self.expired {
            self.expired = false;
            cx.defer_in(window, |this, window, _| this.dismiss(window));
        }
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let title = self.content.title.clone();
        let body = self.content.body.clone();
        let initial = self.content.initial.clone();
        let image = self.content.image.clone();
        let edge = if crate::color::is_dark() {
            gpui::hsla(0., 0., 1., 0.12)
        } else {
            gpui::hsla(0., 0., 0., 0.08)
        };
        div()
            .id("znotice")
            .size_full()
            .overflow_hidden()
            .bg(gpui::transparent_black())
            .child(
                div()
                    .id("zbg")
                    .absolute()
                    .left(px(1.))
                    .top(px(1.))
                    .w(px(350.))
                    .h(px(80.))
                    .rounded(px(CARD_RADIUS))
                    .bg(rgb(paper))
                    .border_1()
                    .border_color(edge)
                    .shadow_md()
                    .overflow_hidden()
                    .occlude()
                    .cursor(gpui::CursorStyle::OpenHand)
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            let origin = window.bounds().origin;
                            let here = (f32::from(origin.x), f32::from(origin.y));
                            this.drag_at = Some(Instant::now());
                            this.last_origin = here;
                            this.moved = false;
                            this.still_since = Instant::now();
                            unlock();
                            window.start_window_move();
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .size_full()
                            .overflow_hidden()
                            .child(avatar_el(initial, image))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .max_w(px(248.))
                                    .mr(px(22.))
                                    .overflow_hidden()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .id("ztitle")
                                            .w_full()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .truncate()
                                            .text_size(px(16.))
                                            .line_height(px(21.))
                                            .font_family(crate::fonts::FAMILY_SEMIBOLD)
                                            .text_color(rgb(ink))
                                            .child(title),
                                    )
                                    .child(
                                        div()
                                            .id("zbody")
                                            .w_full()
                                            .mt(px(6.))
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .truncate()
                                            .text_size(px(14.))
                                            .line_height(px(19.))
                                            .text_color(rgb(muted))
                                            .child(body),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("znotice-close")
                            .absolute()
                            .top(px(2.))
                            .right(px(2.))
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_size(px(14.))
                            .text_color(rgb(0x7f8c99))
                            .occlude()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.dismiss(window);
                                    cx.stop_propagation();
                                }),
                            )
                            .child("×"),
                    ),
            )
    }
}

fn avatar_el(initial: String, image: Option<Arc<RenderImage>>) -> impl IntoElement {
    let letter = image.is_none();
    div()
        .id("zavatar")
        .flex_none()
        .ml(px(8.))
        .mr(px(8.))
        .size(px(52.))
        .when_some(image, |el, img| {
            el.child(
                gpui::img(img)
                    .size(px(52.))
                    .rounded(px(26.))
                    .object_fit(gpui::ObjectFit::Cover),
            )
        })
        .when(letter, |el| el.child(letter_avatar(initial)))
}

fn letter_avatar(initial: String) -> impl IntoElement {
    div()
        .size(px(52.))
        .rounded(px(26.))
        .bg(rgb(0x0068ff))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(0xffffff))
        .text_size(px(20.))
        .font_family(crate::fonts::FAMILY_SEMIBOLD)
        .child(initial)
}

#[cfg(test)]
mod tests {
    use super::screen_origin;

    #[test]
    fn four_screen_corners_use_the_work_area() {
        let (x, y, w, h) = (0.0, 40.0, 1920.0, 1040.0);
        assert_eq!(screen_origin("tl", 0.0, 0.0, x, y, w, h), (5.0, 45.0));
        let (right, top) = screen_origin("tr", 0.0, 0.0, x, y, w, h);
        assert_eq!(right, 1920.0 - super::NOTICE_W - 5.0);
        assert_eq!(top, 45.0);
        let (left, bottom) = screen_origin("bl", 0.0, 0.0, x, y, w, h);
        assert_eq!(left, 5.0);
        assert_eq!(bottom, 40.0 + 1040.0 - super::NOTICE_H - 5.0);
        assert_eq!(screen_origin("br", 0.0, 0.0, x, y, w, h), (right, bottom));
        let (cx, cy) = screen_origin("custom", 10_000.0, -20.0, x, y, w, h);
        assert!(cx <= x + w - super::NOTICE_W);
        assert!(cy >= y);
        assert_eq!(screen_origin("nope", 0.0, 0.0, x, y, w, h), (right, top));
    }

    #[test]
    fn notice_body_is_one_line() {
        assert_eq!(super::notice_line("a\r\nb\nc"), "a  b c");
        assert!(!super::notice_line("dòng\ndài").contains('\n'));
    }
}
