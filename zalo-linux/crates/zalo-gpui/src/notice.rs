//! Screen-corner message popup, separate from the main Zalo window.
//!
//! Official Zalo (`znotification`) is a frameless always-on-top window,
//! 352×82, parked on the primary display work area with a 5 px margin.
//! The four corners here are that work area, plus a dragged custom spot.
//! They are not the corners of the Zalo window. `xfce4-notifyd` ignores
//! x/y, so this window is positioned by us.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use x11rb::connection::Connection;
#[cfg(target_os = "linux")]
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

/// Default popup auto-dismiss time in seconds (matching Electron `popupApprearTime`).
pub const DEFAULT_DURATION_SECS: u64 = 5;

/// Clamp user-configured popup display duration into a safe range (`2..=30` s).
#[must_use]
pub fn clamp_duration_secs(secs: u64) -> u64 {
    secs.clamp(2, 30)
}

/// One popup: title, one-line body, avatar initial, decoded photo, or group collage.
pub struct NoticeContent {
    /// Conversation name, or the sample title.
    pub title: String,
    /// Message line. Newlines already turned into spaces.
    pub body: String,
    /// Initials drawn when neither a photo nor a group collage is available.
    pub initial: String,
    /// Fallback disc colour (`avatar_color` of the thread id).
    pub accent: u32,
    /// Cached single avatar, when the shell already decoded one.
    pub image: Option<Arc<RenderImage>>,
    /// Overlapping member collage for groups without a custom group photo.
    pub(crate) collage: Vec<crate::sidebar::CollageFace>,
    /// Thread whose title/photo/collage should patch while this popup is open.
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
    #[cfg(not(target_os = "linux"))]
    {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
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
    duration: Duration,
) {
    let duration = Duration::from_secs(clamp_duration_secs(duration.as_secs().max(2)));
    let (wx, wy, ww, wh) = area_for(cx, scale);
    let (x, y) = screen_origin(corner, saved_x, saved_y, wx, wy, ww, wh);
    set_target(x, y, true);
    if let Some(handle) = open_slot().lock().ok().and_then(|guard| *guard) {
        if handle.read(cx).is_ok() {
            let _ = handle.update(cx, |view, _, cx| {
                view.replace(content, duration, cx);
                cx.notify();
            });
            crate::x11_frame::move_notice_to(x, y, scale);
            return;
        }
        if let Ok(mut guard) = open_slot().lock() {
            *guard = None;
        }
    }
    // Stamp non-focusable notification hints before the WM handles MapRequest.
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
            // `PopUp` sets `_NET_WM_WINDOW_TYPE_NOTIFICATION` on X11 before
            // `map_window()`, matching Electron `znotification` (no focus
            // stealing, no server frame, no center-screen placement).
            kind: WindowKind::PopUp,
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
            // Ensure `WM_HINTS.input=0`, `_NET_WM_USER_TIME=0`, `_NET_WM_STATE`,
            // corner position, and 4 px input mask without unmapping/remapping.
            crate::x11_frame::park_notice(window.scale_factor());
            window.set_client_inset(px(0.));
            window.set_background_appearance(WindowBackgroundAppearance::Transparent);
            cx.new(|cx| NoticeView::new(content, duration, cx))
        },
    );
    if let Ok(handle) = opened {
        if let Ok(mut guard) = open_slot().lock() {
            *guard = Some(handle);
        }
    }
}

/// Update the open popup's title, decoded photo, or group collage while it is
/// still showing `thread_key`. Does not restart the countdown progress bar.
pub(crate) fn patch_notice(
    cx: &mut App,
    thread_key: &str,
    title: Option<String>,
    image: Option<Arc<RenderImage>>,
    collage: Vec<crate::sidebar::CollageFace>,
) -> bool {
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
            if view.expired || view.content.thread_key != key {
                return false;
            }
            let mut changed = false;
            if let Some(name) = title {
                if !name.is_empty() && name != view.content.title {
                    view.content.initial = crate::sidebar::avatar_initials(&name);
                    view.content.title = name;
                    changed = true;
                }
            }
            if let Some(img) = image {
                if view.content.image.is_none() {
                    view.content.image = Some(img);
                    view.content.collage.clear();
                    changed = true;
                }
            } else if view.content.image.is_none() && !collage.is_empty() {
                view.content.collage = collage;
                changed = true;
            }
            if changed {
                cx.notify();
            }
            changed
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
    crate::x11_frame::move_notice_to(x, y, scale);
    let _ = handle.update(cx, |_, _, cx| cx.notify());
}

/// Minimum pointer travel (logical px) before a press turns into a drag.
const DRAG_THRESHOLD: f32 = 5.0;

/// Inner card width (`#zbg`, inside 1 px outer margin on each side).
const CARD_W: f32 = 350.0;
/// Inner card height (`#zbg`).
const CARD_H: f32 = 80.0;
/// Countdown progress bar height along the bottom edge of `#zbg`.
const PROGRESS_H: f32 = 3.0;

struct NoticeView {
    content: NoticeContent,
    gen: u64,
    duration: Duration,
    remaining: Duration,
    last_tick: Instant,
    expired: bool,
    hovered: bool,
    /// Local mouse-down position inside the card (logical px).
    press_local: Option<(f32, f32)>,
    /// X11 root pointer at mouse-down (`None` on Wayland).
    press_root: Option<(i32, i32)>,
    /// Window origin (logical px) when the drag began.
    drag_origin: (f32, f32),
    /// Latest window origin (logical px) during an X11 drag.
    drag_pos: (f32, f32),
    /// Work-area bounds cached on mouse-down so dragging does not re-query X11.
    drag_area: (f32, f32, f32, f32),
    /// True once pointer travel exceeded [`DRAG_THRESHOLD`].
    dragging: bool,
    /// Wayland fallback drag tracking (`start_window_move`).
    drag_at: Option<Instant>,
    last_origin: (f32, f32),
    moved: bool,
    still_since: Instant,
}

impl NoticeView {
    fn new(content: NoticeContent, duration: Duration, cx: &mut Context<Self>) -> Self {
        let now = Instant::now();
        let view = Self {
            content,
            gen: 1,
            duration,
            remaining: duration,
            last_tick: now,
            expired: false,
            hovered: false,
            press_local: None,
            press_root: None,
            drag_origin: (0.0, 0.0),
            drag_pos: (0.0, 0.0),
            drag_area: (0.0, 0.0, 1920.0, 1080.0),
            dragging: false,
            drag_at: None,
            last_origin: (0.0, 0.0),
            moved: false,
            still_since: now,
        };
        view.arm_countdown(1, cx);
        view
    }

    fn replace(&mut self, content: NoticeContent, duration: Duration, cx: &mut Context<Self>) {
        self.content = content;
        self.gen = self.gen.wrapping_add(1);
        self.duration = duration;
        self.remaining = duration;
        self.last_tick = Instant::now();
        let gen = self.gen;
        self.arm_countdown(gen, cx);
    }

    /// Fraction of display time left in `0.0..=1.0`, used for the bottom
    /// countdown progress bar.
    fn progress_ratio(&self) -> f32 {
        let total = self.duration.as_secs_f32();
        if total <= 0.0 {
            return 0.0;
        }
        (self.remaining.as_secs_f32() / total).clamp(0.0, 1.0)
    }

    /// Smooth ~30 FPS countdown ticker. Pauses while hovered or dragged and
    /// dismisses the popup once `remaining` reaches zero.
    fn arm_countdown(&self, gen: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            gpui::Timer::after(Duration::from_millis(33)).await;
            let keep = this.update(cx, |view, cx| {
                if view.gen != gen || view.expired {
                    return false;
                }
                let now = Instant::now();
                let dt = now.saturating_duration_since(view.last_tick);
                view.last_tick = now;
                if view.hovered || view.dragging || view.drag_at.is_some() {
                    return true;
                }
                view.remaining = view.remaining.saturating_sub(dt);
                if view.remaining.is_zero() {
                    view.expired = true;
                    cx.notify();
                    return false;
                }
                cx.notify();
                true
            });
            if !keep.unwrap_or(false) {
                break;
            }
        })
        .detach();
    }

    fn dismiss(&mut self, window: &mut Window) {
        self.press_local = None;
        self.press_root = None;
        self.dragging = false;
        if let Ok(mut guard) = open_slot().lock() {
            *guard = None;
        }
        unlock();
        window.remove_window();
    }

    fn finish_x11_drag(&mut self, cx: &mut Context<Self>) {
        let was_x11_drag = self.dragging && self.press_root.is_some();
        let pos = self.drag_pos;
        self.press_local = None;
        self.press_root = None;
        self.dragging = false;
        if was_x11_drag {
            remember_custom(pos.0, pos.1, cx);
        }
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
        crate::x11_frame::sync_notice(window.scale_factor());
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
        let accent = self.content.accent;
        let image = self.content.image.clone();
        let collage = self.content.collage.clone();
        let progress_w = (CARD_W * self.progress_ratio()).clamp(0.0, CARD_W);
        let progress_track = if crate::color::is_dark() {
            gpui::hsla(0., 0., 1., 0.08)
        } else {
            gpui::hsla(0., 0., 0., 0.06)
        };
        let cursor = if self.dragging {
            gpui::CursorStyle::ClosedHand
        } else if self.content.thread_key.is_empty() {
            gpui::CursorStyle::OpenHand
        } else {
            gpui::CursorStyle::PointingHand
        };
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
                    .w(px(CARD_W))
                    .h(px(CARD_H))
                    .rounded(px(CARD_RADIUS))
                    .bg(rgb(paper))
                    .border_1()
                    .border_color(edge)
                    .shadow_md()
                    .overflow_hidden()
                    .occlude()
                    .cursor(cursor)
                    .on_hover(cx.listener(|this, hovered: &bool, _, _| {
                        this.hovered = *hovered;
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                            let scale = window.scale_factor();
                            let win_origin = crate::x11_frame::notice_root_origin(scale)
                                .or_else(locked_origin)
                                .unwrap_or_else(|| {
                                    let o = window.bounds().origin;
                                    (f32::from(o.x), f32::from(o.y))
                                });
                            this.press_local = Some((f32::from(e.position.x), f32::from(e.position.y)));
                            this.press_root = crate::x11_frame::query_pointer_root();
                            this.drag_origin = win_origin;
                            this.drag_pos = win_origin;
                            this.drag_area = area_for(cx, scale);
                            this.dragging = false;
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, window, cx| {
                        let Some((lx, ly)) = this.press_local else {
                            return;
                        };
                        if e.pressed_button != Some(gpui::MouseButton::Left) {
                            this.finish_x11_drag(cx);
                            cx.notify();
                            return;
                        }
                        let scale = window.scale_factor().max(0.5);
                        if let Some((rx0, ry0)) = this.press_root {
                            let Some((rx, ry)) = crate::x11_frame::query_pointer_root() else {
                                return;
                            };
                            let dx = (rx - rx0) as f32 / scale;
                            let dy = (ry - ry0) as f32 / scale;
                            if !this.dragging {
                                if dx.abs().max(dy.abs()) <= DRAG_THRESHOLD {
                                    return;
                                }
                                this.dragging = true;
                                unlock();
                                cx.notify();
                            }
                            let (wx, wy, ww, wh) = this.drag_area;
                            let max_x = (wx + ww - NOTICE_W).max(wx);
                            let max_y = (wy + wh - NOTICE_H).max(wy);
                            let nx = (this.drag_origin.0 + dx).clamp(wx, max_x);
                            let ny = (this.drag_origin.1 + dy).clamp(wy, max_y);
                            this.drag_pos = (nx, ny);
                            crate::x11_frame::move_notice_to(nx, ny, scale);
                        } else {
                            let dx = f32::from(e.position.x) - lx;
                            let dy = f32::from(e.position.y) - ly;
                            if !this.dragging && dx.abs().max(dy.abs()) > DRAG_THRESHOLD {
                                this.dragging = true;
                                let origin = window.bounds().origin;
                                let here = (f32::from(origin.x), f32::from(origin.y));
                                this.drag_at = Some(Instant::now());
                                this.last_origin = here;
                                this.moved = false;
                                this.still_since = Instant::now();
                                unlock();
                                window.start_window_move();
                                cx.notify();
                            }
                        }
                    }))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseUpEvent, window, cx| {
                            let pressed = this.press_local.is_some();
                            let dragged = this.dragging;
                            this.finish_x11_drag(cx);
                            if pressed && !dragged && !this.content.thread_key.is_empty() {
                                let key = this.content.thread_key.clone();
                                this.dismiss(window);
                                cx.defer(move |cx| {
                                    crate::shell::open_notice_thread(&key, cx);
                                });
                            } else {
                                cx.notify();
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_up_out(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                            this.finish_x11_drag(cx);
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .size_full()
                            .pb(px(PROGRESS_H))
                            .overflow_hidden()
                            .child(avatar_el(initial, accent, image, collage))
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
                            .id("znotice-progress")
                            .absolute()
                            .left(px(0.))
                            .bottom(px(0.))
                            .w(px(CARD_W))
                            .h(px(PROGRESS_H))
                            .bg(progress_track)
                            .child(
                                div()
                                    .h_full()
                                    .w(px(progress_w))
                                    .bg(rgb(0x0068ff)),
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
                            .text_color(rgb(muted))
                            .occlude()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.dismiss(window);
                                    cx.stop_propagation();
                                }),
                            )
                            .on_mouse_up(
                                gpui::MouseButton::Left,
                                |_, _, cx| {
                                    cx.stop_propagation();
                                },
                            )
                            .child(crate::fonts::icon_el(crate::fonts::ic::CLOSE_LINE, "×", 14.)),
                    ),
            )
    }
}

fn avatar_el(
    initial: String,
    accent: u32,
    image: Option<Arc<RenderImage>>,
    collage: Vec<crate::sidebar::CollageFace>,
) -> impl IntoElement {
    let show_collage = image.is_none() && !collage.is_empty();
    let show_letter = image.is_none() && collage.is_empty();
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
        .when(show_collage, |el| {
            el.child(crate::sidebar::group_collage(&collage, 52.0))
        })
        .when(show_letter, |el| el.child(letter_avatar(initial, accent)))
}

fn letter_avatar(initial: String, accent: u32) -> impl IntoElement {
    let fill = if accent == 0 { 0x0068ff } else { accent };
    div()
        .size(px(52.))
        .rounded(px(26.))
        .bg(rgb(fill))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(0xffffff))
        .text_size(px(19.))
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

    #[test]
    fn notice_duration_clamps_to_safe_bounds() {
        assert_eq!(super::clamp_duration_secs(0), 2);
        assert_eq!(super::clamp_duration_secs(5), 5);
        assert_eq!(super::clamp_duration_secs(120), 30);
    }
}
