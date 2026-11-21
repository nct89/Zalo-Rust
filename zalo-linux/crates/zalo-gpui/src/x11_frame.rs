//! Rounded X11 windows on WMs that do not advertise `_GTK_FRAME_EXTENTS`.
//!
//! GPUI keeps client decorations only when a compositor is present *and*
//! `_GTK_FRAME_EXTENTS` is in `_NET_SUPPORTED`. XFCE xfwm fails the second
//! check and GPUI falls back to a square server frame. This module finds our
//! own top-level windows by `_NET_WM_PID` (a second x11rb connection — GPUI's
//! X11 `window_handle()` panics) and:
//!
//! * writes `_MOTIF_WM_HINTS` with decorations off, so the in-app title bar
//!   is the only one;
//! * sets the SHAPE masks from the frame radius, on the client and on the
//!   xfwm frame behind it. A compositor (`_NET_WM_CM_S{n}` has an owner)
//!   blends the antialiased pixels, so the bounding mask only covers the
//!   curve and the square plate stays clipped. Without a compositor the
//!   mask is the tight 1 px staircase: alpha is ignored, and a full bounding
//!   rectangle paints the corner wedges as a square plate.
//!
//! Wayland (`DISPLAY` unset) does nothing: the transparent buffer plus the
//! per-corner radius already punches the corners. A window whose
//! `_NET_WM_NAME` starts with `Zalo Call` keeps the OS frame so the live call
//! can still be closed. A window titled `Zalo Notice` keeps decorations off,
//! uses a 4 px input curve (the official card radius), and is raised above
//! other windows. Maximized windows get a square mask.
//!
//! Do not call [`gpui::Window::request_decorations`] from here. On this class
//! of WM that call writes the server-side motif hint back.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::shape::{self, SK, SO};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ClipOrdering, ConfigureWindowAux, ConnectionExt as _, EventMask,
    ExposeEvent, MapState, PropMode, Rectangle, EXPOSE_EVENT,
};
use x11rb::wrapper::ConnectionExt as _;

static COMPOSITED_INIT: OnceLock<()> = OnceLock::new();
static COMPOSITED_LIVE: AtomicBool = AtomicBool::new(true);

/// Returns `true` when the active desktop session composites 32-bit ARGB alpha
/// (always `true` on Wayland and in tests; on X11 tracks `_NET_WM_CM_Sn`).
///
/// Mirrors GTK3/GTK4/libadwaita/Chromium/Firefox: composited desktops get
/// GPU-antialiased rounded corners, while non-composited X11 (e.g.
/// `xfwm4 --compositor=off`, i3, Openbox without picom) uses crisp square
/// corners with a continuous 1 px frame so corners never show 1-bit jaggedness.
#[must_use]
pub fn is_composited() -> bool {
    if cfg!(test)
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("DISPLAY").is_none()
    {
        return true;
    }
    COMPOSITED_INIT.get_or_init(|| {
        let active = x11rb::connect(None)
            .ok()
            .is_some_and(|(conn, screen)| compositor_present(&conn, screen));
        COMPOSITED_LIVE.store(active, Ordering::Relaxed);
    });
    COMPOSITED_LIVE.load(Ordering::Relaxed)
}

/// Last shape applied to one X window. `motif` stays set across resizes.
#[derive(Clone, Copy)]
struct ShapeKey {
    width: u16,
    height: u16,
    radius: u16,
    /// Bounding mask follows the antialiased cover instead of the tight stair.
    composited: bool,
    motif: bool,
    raised: bool,
}

struct State {
    scanned: Option<Instant>,
    /// Last time a locked notice was pushed to its screen corner.
    snapped: Option<Instant>,
    /// Last seen `(width, height, maximized, scale_x100)` for the active window.
    last_sig: Option<(i32, i32, bool, u16)>,
    /// Time of the last geometry/window change so we stop polling X11 once settled.
    sig_changed_at: Instant,
    shapes: HashMap<u32, ShapeKey>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            scanned: None,
            snapped: None,
            last_sig: None,
            sig_changed_at: Instant::now(),
            shapes: HashMap::new(),
        })
    })
}

/// Wait up to `timeout_ms` for all non-notice windows owned by this process
/// to be unmapped/iconified by the X11 WM, then pause briefly for the compositor
/// to repaint the desktop underneath before grabbing a screenshot without Zalo.
pub fn wait_app_windows_hidden(timeout_ms: u64) {
    if cfg!(test)
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("DISPLAY").is_none()
    {
        std::thread::sleep(Duration::from_millis(timeout_ms.min(250)));
        return;
    }
    let Ok((conn, screen)) = x11rb::connect(None) else {
        std::thread::sleep(Duration::from_millis(250));
        return;
    };
    let Some(root) = conn.setup().roots.get(screen).map(|s| s.root) else {
        return;
    };
    let intern = |name: &[u8]| -> Option<u32> {
        conn.intern_atom(false, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
            .filter(|a| *a != 0)
    };
    let Some(wm_pid) = intern(b"_NET_WM_PID") else {
        std::thread::sleep(Duration::from_millis(250));
        return;
    };
    let wm_name = intern(b"_NET_WM_NAME").unwrap_or(0);
    let utf8 = intern(b"UTF8_STRING").unwrap_or(0);
    let pid = std::process::id();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let mut any_visible = false;
        for client in owned_clients(&conn, root, wm_pid, pid) {
            let name = if wm_name != 0 && utf8 != 0 {
                window_name(&conn, client, wm_name, utf8)
            } else {
                String::new()
            };
            if name.starts_with("Zalo Notice") {
                continue;
            }
            if conn
                .get_window_attributes(client)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|a| a.map_state == MapState::VIEWABLE)
            {
                any_visible = true;
                break;
            }
        }
        if !any_visible || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Give xfwm4/compositor one short cycle to finish repainting the area behind Zalo.
    std::thread::sleep(Duration::from_millis(120));
}

/// Reset the settle timer when a new window is about to open so `sync_window`
/// never skips applying `_MOTIF_WM_HINTS` / `XShape` to reopened windows.
pub fn reset_settle() {
    if let Ok(mut guard) = state().lock() {
        guard.scanned = None;
        guard.last_sig = None;
        guard.sig_changed_at = Instant::now();
    }
}

/// Stamp `_MOTIF_WM_HINTS` (decorations off) on a brand-new CSD window *before*
/// xfwm4 processes `MapRequest`, preventing xfwm4 from painting a server frame
/// and then clearing the composited backing pixmap to black when stripping it.
pub fn anticipate_csd_window() {
    reset_settle();
    if cfg!(test)
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("DISPLAY").is_none()
    {
        return;
    }
    let known: HashSet<u32> = state()
        .lock()
        .ok()
        .map(|g| g.shapes.keys().copied().collect())
        .unwrap_or_default();
    let _ = std::thread::Builder::new()
        .name("zalo-csd-frame".into())
        .spawn(move || {
            let Some((conn, screen)) = x11rb::connect(None).ok() else {
                return;
            };
            let Some(root) = conn.setup().roots.get(screen).map(|s| s.root) else {
                return;
            };
            let intern = |name: &[u8]| -> Option<u32> {
                conn.intern_atom(false, name)
                    .ok()?
                    .reply()
                    .ok()
                    .map(|r| r.atom)
                    .filter(|a| *a != 0)
            };
            let Some(wm_pid) = intern(b"_NET_WM_PID") else {
                return;
            };
            let Some(wm_name) = intern(b"_NET_WM_NAME") else {
                return;
            };
            let Some(utf8) = intern(b"UTF8_STRING") else {
                return;
            };
            let Some(motif) = intern(b"_MOTIF_WM_HINTS") else {
                return;
            };
            let pid = std::process::id();
            for _ in 0..35 {
                for window in owned_clients(&conn, root, wm_pid, pid) {
                    if known.contains(&window) {
                        continue;
                    }
                    let name = window_name(&conn, window, wm_name, utf8);
                    if name.starts_with("Zalo Call") || name.starts_with("Zalo Notice") {
                        continue;
                    }
                    if write_motif(&conn, window, motif) {
                        let _ = conn.flush();
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(3));
            }
        });
}

/// Apply the OS frame for every Zalo window of this process.
///
/// Throttled: `wrap_csd` runs on every frame. Unit tests never open a display.
pub fn sync(scale: f32) {
    let _ = sync_window(0, 0, false, scale);
}

/// Geometry-aware X11 frame sync. Returns `true` when `_MOTIF_WM_HINTS` or
/// `XShape` was newly applied so the caller can immediately refresh the
/// composited surface (preventing a black startup frame on XFCE/xfwm4).
///
/// Once the window's geometry has been shaped and settled for 1.5 s, subsequent
/// calls with identical `(width, height, maximized, scale)` return immediately
/// with zero X11 syscalls (avoiding frame stalls during scrolling).
pub fn sync_window(width: i32, height: i32, maximized: bool, scale: f32) -> bool {
    if cfg!(test) || std::env::var_os("DISPLAY").is_none() {
        return false;
    }
    snap_locked_if_due(scale);
    let Ok(mut guard) = state().try_lock() else {
        return false;
    };
    let now = Instant::now();
    let sig = (
        width,
        height,
        maximized,
        (scale.max(1.0) * 100.0).round() as u16,
    );
    let sig_changed = width > 0 && height > 0 && guard.last_sig != Some(sig);
    if sig_changed {
        guard.last_sig = Some(sig);
        guard.sig_changed_at = now;
    }
    let settled = !sig_changed
        && !guard.shapes.is_empty()
        && now.duration_since(guard.sig_changed_at) >= Duration::from_millis(1500);
    if settled {
        return false;
    }
    let throttle = if guard.shapes.is_empty() || sig_changed {
        Duration::from_millis(50)
    } else {
        Duration::from_millis(250)
    };
    if !sig_changed
        && guard
            .scanned
            .is_some_and(|at| now.duration_since(at) < throttle)
    {
        return false;
    }
    guard.scanned = Some(now);
    let known = guard.shapes.clone();
    drop(guard);

    let Some((shapes, changed)) = apply(scale, &known) else {
        return false;
    };
    if let Ok(mut guard) = state().lock() {
        guard.shapes = shapes;
    }
    changed
}

/// Rectangles that fill a `width`×`height` window with a circular corner of
/// `radius` device pixels. `radius == 0` (or too big for the window) is one
/// full rectangle, which clears a previous rounded mask.
///
/// The inset is the nearest pixel. That is the mask used when nothing can
/// blend a partial pixel, so the stair does not leave a dark crumb outside
/// the curve.
#[must_use]
pub fn rounded_rows(width: u16, height: u16, radius: u16) -> Vec<Rectangle> {
    rounded_rows_inset(width, height, radius, round_inset)
}

/// Like [`rounded_rows`], but every pixel the disk touches stays inside.
///
/// A compositor blends the buffer's antialiased fringe. This cover keeps
/// that fringe and still clips the square plate outside it.
#[must_use]
pub fn rounded_rows_cover(width: u16, height: u16, radius: u16) -> Vec<Rectangle> {
    rounded_rows_inset(width, height, radius, round_inset_cover)
}

fn rounded_rows_inset(
    width: u16,
    height: u16,
    radius: u16,
    inset_of: fn(i32, i32) -> i32,
) -> Vec<Rectangle> {
    let w = i32::from(width);
    let h = i32::from(height);
    let r = i32::from(radius);
    if w <= 0 || h <= 0 || r <= 0 || w <= r * 2 || h <= r * 2 {
        return vec![Rectangle {
            x: 0,
            y: 0,
            width,
            height,
        }];
    }
    let mut rows = Vec::with_capacity(radius as usize * 2 + 1);
    rows.push(Rectangle {
        x: 0,
        y: r as i16,
        width,
        height: (h - r * 2) as u16,
    });
    for y in 0..r {
        let inset = inset_of(y, r).clamp(0, r);
        let row_w = (w - inset * 2).max(0) as u16;
        if row_w == 0 {
            continue;
        }
        let x = inset as i16;
        rows.push(Rectangle {
            x,
            y: y as i16,
            width: row_w,
            height: 1,
        });
        rows.push(Rectangle {
            x,
            y: (h - 1 - y) as i16,
            width: row_w,
            height: 1,
        });
    }
    rows
}

/// How many pixels to skip at the start of row `y` of a quarter circle.
fn round_inset(y: i32, radius: i32) -> i32 {
    if radius <= 0 || y < 0 || y >= radius {
        return 0;
    }
    let dy = radius as f32 - (y as f32 + 0.5);
    let inside = (radius as f32 * radius as f32 - dy * dy).max(0.0).sqrt();
    (radius as f32 - inside).round() as i32
}

/// Inset that still includes every pixel the disk touches.
fn round_inset_cover(y: i32, radius: i32) -> i32 {
    if radius <= 0 || y < 0 || y >= radius {
        return 0;
    }
    let dy = (radius - (y + 1)).max(0) as f32;
    let inside = (radius as f32 * radius as f32 - dy * dy).max(0.0).sqrt();
    (radius as f32 - inside).floor() as i32
}

/// `_NET_WM_CM_Sn` owner. Screen number, not the root window id.
fn compositor_present(conn: &impl Connection, screen: usize) -> bool {
    let name = format!("_NET_WM_CM_S{screen}");
    let Ok(cookie) = conn.intern_atom(false, name.as_bytes()) else {
        return false;
    };
    let Ok(atom) = cookie.reply() else {
        return false;
    };
    if atom.atom == 0 {
        return false;
    }
    let Ok(cookie) = conn.get_selection_owner(atom.atom) else {
        return false;
    };
    cookie.reply().ok().is_some_and(|owner| owner.owner != 0)
}

fn apply(scale: f32, known: &HashMap<u32, ShapeKey>) -> Option<(HashMap<u32, ShapeKey>, bool)> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let atom = |name: &str| -> Option<u32> {
        conn.intern_atom(false, name.as_bytes())
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
    };
    let client_list = atom("_NET_CLIENT_LIST")?;
    let wm_pid = atom("_NET_WM_PID")?;
    let wm_name = atom("_NET_WM_NAME")?;
    let wm_state = atom("_NET_WM_STATE")?;
    let max_v = atom("_NET_WM_STATE_MAXIMIZED_VERT")?;
    let max_h = atom("_NET_WM_STATE_MAXIMIZED_HORZ")?;
    let motif = atom("_MOTIF_WM_HINTS")?;
    let utf8 = atom("UTF8_STRING")?;
    let above = atom("_NET_WM_STATE_ABOVE");
    let skip_taskbar = atom("_NET_WM_STATE_SKIP_TASKBAR");
    let skip_pager = atom("_NET_WM_STATE_SKIP_PAGER");
    let ids = prop32(&conn, root, client_list)?;
    let pid = std::process::id();
    let composited = compositor_present(&conn, screen);
    let _ = COMPOSITED_INIT.set(());
    COMPOSITED_LIVE.store(composited, Ordering::Relaxed);
    let radius_px = if composited {
        (zalo_design::LINUX_FRAME_RADIUS * scale.max(1.0))
            .round()
            .clamp(1.0, 64.0) as u16
    } else {
        0
    };
    let mut shapes = HashMap::new();
    let mut changed = false;
    let mut newly_framed: Vec<(u32, u16, u16)> = Vec::new();
    for window in ids {
        let Some(owner) = prop32(&conn, window, wm_pid).and_then(|v| v.first().copied()) else {
            continue;
        };
        if owner != pid {
            continue;
        }
        let name = window_name(&conn, window, wm_name, utf8);
        if name.starts_with("Zalo Call") {
            // Server frame. GPUI's window handle panics on X11, so the icon
            // is set on the window we already found by `_NET_WM_PID`.
            pin_call_icon(&conn, window);
            continue;
        }
        let geo = match conn.get_geometry(window) {
            Ok(cookie) => match cookie.reply() {
                Ok(geo) => geo,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        if geo.width == 0 || geo.height == 0 {
            continue;
        }
        let name_notice = name.starts_with("Zalo Notice");
        let maximized = prop32(&conn, window, wm_state)
            .is_some_and(|states| states.contains(&max_v) && states.contains(&max_h));
        let radius = if name_notice {
            if composited {
                (4.0 * scale.max(1.0)).round().clamp(1.0, 32.0) as u16
            } else {
                0
            }
        } else if maximized {
            0
        } else {
            radius_px
        };
        let previous = known.get(&window).copied();
        let motif_done = previous.is_some_and(|prev| prev.motif);
        if !motif_done {
            if !write_motif(&conn, window, motif) {
                continue;
            }
            changed = true;
        }
        let raised = if name_notice {
            let already = previous.is_some_and(|prev| prev.raised);
            if !already {
                write_notice_no_focus(&conn, window);
            }
            already || raise_notice(&conn, root, window, wm_state, above, skip_taskbar, skip_pager)
        } else {
            false
        };
        let key = ShapeKey {
            width: geo.width,
            height: geo.height,
            radius,
            composited,
            motif: true,
            raised,
        };
        let same_shape = previous.is_some_and(|prev| {
            prev.width == key.width
                && prev.height == key.height
                && prev.radius == key.radius
                && prev.composited == key.composited
        });
        if !same_shape {
            if !write_shape(&conn, window, geo.width, geo.height, radius, composited) {
                continue;
            }
            changed = true;
        }
        if !same_shape || !motif_done {
            // The frame is a sibling-sized parent. Its bounding rectangle stays
            // square even after the client is shaped, and with the compositor off
            // that square is the plate under the rounded UI.
            shape_frame(
                &conn, root, window, geo.x, geo.y, geo.width, geo.height, radius, composited,
            );
            send_expose(&conn, window, geo.width, geo.height);
            if !motif_done {
                newly_framed.push((window, geo.width, geo.height));
            }
        }
        shapes.insert(window, key);
    }
    let _ = conn.flush();
    if !newly_framed.is_empty() {
        // xfwm4 strips the server frame asynchronously and clears the newly
        // redirected composited pixmap to black without re-sending Expose;
        // follow-up Expose events force GPUI's X11 refresh loop to present.
        std::thread::spawn(move || {
            for delay_ms in [120u64, 350] {
                std::thread::sleep(Duration::from_millis(delay_ms));
                let Ok((conn, _)) = x11rb::connect(None) else {
                    break;
                };
                for &(win, w, h) in &newly_framed {
                    send_expose(&conn, win, w, h);
                }
                let _ = conn.flush();
            }
        });
    }
    Some((shapes, changed))
}

fn send_expose(conn: &impl Connection, window: u32, width: u16, height: u16) {
    let ev = ExposeEvent {
        response_type: EXPOSE_EVENT,
        sequence: 0,
        window,
        x: 0,
        y: 0,
        width,
        height,
        count: 0,
    };
    let _ = conn.send_event(false, window, EventMask::EXPOSURE, ev);
}

/// `_MOTIF_WM_HINTS`: flags = decorations, decorations = none.
fn write_motif(conn: &impl Connection, window: u32, motif: u32) -> bool {
    let hints: [u32; 5] = [1 << 1, 0, 0, 0, 0];
    conn.change_property32(PropMode::REPLACE, window, motif, motif, &hints)
        .is_ok()
}

fn write_shape(
    conn: &impl Connection,
    window: u32,
    width: u16,
    height: u16,
    radius: u16,
    composited: bool,
) -> bool {
    let (bounding, input) = shape_masks(width, height, radius, composited);
    write_shape_pair(conn, window, &bounding, &input)
}

/// Bounding clips pixels; input clips clicks.
///
/// * **Composited X11** (`_NET_WM_CM_Sn` active): leave `SK::BOUNDING` as the
///   full rectangle so the 32-bit ARGB buffer paints GPU-antialiased rounded
///   corners and smooth WM drop shadows without any 1-bit `XShape` staircase,
///   while `SK::INPUT` clips pointer hit-testing to the rounded curve.
/// * **Non-composited X11**: both masks are the full rectangle (`radius == 0`),
///   matching GTK3/GTK4/Chromium/Firefox so corners stay razor-sharp.
fn shape_masks(
    width: u16,
    height: u16,
    radius: u16,
    composited: bool,
) -> (Vec<Rectangle>, Vec<Rectangle>) {
    let full = vec![Rectangle {
        x: 0,
        y: 0,
        width,
        height,
    }];
    if composited && radius > 0 {
        let input = rounded_rows(width, height, radius);
        (full, input)
    } else {
        (full.clone(), full)
    }
}

fn write_shape_pair(
    conn: &impl Connection,
    window: u32,
    bounding: &[Rectangle],
    input: &[Rectangle],
) -> bool {
    for (kind, mask) in [(SK::BOUNDING, bounding), (SK::INPUT, input)] {
        let Ok(cookie) = shape::rectangles(
            conn,
            SO::SET,
            kind,
            ClipOrdering::UNSORTED,
            window,
            0,
            0,
            mask,
        ) else {
            return false;
        };
        if cookie.check().is_err() {
            return false;
        }
    }
    true
}

/// Clip the xfwm frame that reparents `window`. Stops at the root, and skips
/// a parent that is more than a border larger than the client.
fn shape_frame(
    conn: &impl Connection,
    root: u32,
    window: u32,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    radius: u16,
    composited: bool,
) {
    let Ok(cookie) = conn.query_tree(window) else {
        return;
    };
    let Ok(tree) = cookie.reply() else {
        return;
    };
    let parent = tree.parent;
    if parent == root || parent == 0 {
        return;
    }
    let Ok(cookie) = conn.get_geometry(parent) else {
        return;
    };
    let Ok(parent_geo) = cookie.reply() else {
        return;
    };
    if parent_geo.width == 0
        || parent_geo.height == 0
        || parent_geo.width > width.saturating_add(16)
        || parent_geo.height > height.saturating_add(16)
    {
        return;
    }
    let (mut bounding, mut input) = shape_masks(width, height, radius, composited);
    if x != 0 || y != 0 {
        for row in bounding.iter_mut().chain(input.iter_mut()) {
            row.x = row.x.saturating_add(x);
            row.y = row.y.saturating_add(y);
        }
    }
    let _ = write_shape_pair(conn, parent, &bounding, &input);
}

/// Mark a notification window as non-focusable, frameless, and typed as
/// `_NET_WM_WINDOW_TYPE_NOTIFICATION` (matching Electron `focusable: false`,
/// `type: 'notification'`, `showInactive()`).
///
/// * `WM_HINTS` with `InputHint = 1, input = 0` (ICCCM No Input model) tells
///   every X11 WM never to grant keyboard focus to this popup.
/// * `_NET_WM_USER_TIME = 0` (EWMH) requests no initial focus on map.
/// * `_NET_WM_WINDOW_TYPE_NOTIFICATION` keeps xfwm4/KWin/Mutter from decorating
///   or centering the popup or stealing focus.
/// * `_MOTIF_WM_HINTS` keeps decorations off on WMs that ignore window type.
fn write_notice_no_focus(conn: &impl Connection, window: u32) {
    // ICCCM WM_HINTS: 9 CARD32s; flags = InputHint (1), input = False (0).
    let wm_hints: [u32; 9] = [1, 0, 0, 0, 0, 0, 0, 0, 0];
    let _ = conn.change_property32(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_HINTS,
        AtomEnum::WM_HINTS,
        &wm_hints,
    );
    let intern = |name: &[u8]| -> Option<u32> {
        conn.intern_atom(false, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
            .filter(|a| *a != 0)
    };
    if let Some(user_time) = intern(b"_NET_WM_USER_TIME") {
        let _ = conn.change_property32(
            PropMode::REPLACE,
            window,
            user_time,
            AtomEnum::CARDINAL,
            &[0u32],
        );
    }
    if let (Some(wm_type), Some(notif_type)) = (
        intern(b"_NET_WM_WINDOW_TYPE"),
        intern(b"_NET_WM_WINDOW_TYPE_NOTIFICATION"),
    ) {
        let _ = conn.change_property32(
            PropMode::REPLACE,
            window,
            wm_type,
            AtomEnum::ATOM,
            &[notif_type],
        );
    }
    if let Some(motif) = intern(b"_MOTIF_WM_HINTS") {
        let _ = write_motif(conn, window, motif);
    }
}

/// Apply all non-focusable notification properties plus `_NET_WM_STATE`
/// (`ABOVE` + `SKIP_TASKBAR` + `SKIP_PAGER`).
fn configure_notice_hints(conn: &impl Connection, root: u32, window: u32, mapped: bool) {
    write_notice_no_focus(conn, window);
    let intern = |name: &[u8]| -> Option<u32> {
        conn.intern_atom(false, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
            .filter(|a| *a != 0)
    };
    let Some(wm_state) = intern(b"_NET_WM_STATE") else {
        return;
    };
    let above = intern(b"_NET_WM_STATE_ABOVE");
    let skip_taskbar = intern(b"_NET_WM_STATE_SKIP_TASKBAR");
    let skip_pager = intern(b"_NET_WM_STATE_SKIP_PAGER");
    if mapped {
        let _ = raise_notice(conn, root, window, wm_state, above, skip_taskbar, skip_pager);
    } else {
        let states: Vec<u32> = [above, skip_taskbar, skip_pager]
            .into_iter()
            .flatten()
            .collect();
        if !states.is_empty() {
            let _ = conn.change_property32(
                PropMode::REPLACE,
                window,
                wm_state,
                AtomEnum::ATOM,
                &states,
            );
        }
    }
}

/// `_NET_WM_STATE_ADD` for above + skip-taskbar + skip-pager. `0` means that atom is absent.
fn raise_notice(
    conn: &impl Connection,
    root: u32,
    window: u32,
    wm_state: u32,
    above: Option<u32>,
    skip_taskbar: Option<u32>,
    skip_pager: Option<u32>,
) -> bool {
    let above = above.unwrap_or(0);
    let skip_taskbar = skip_taskbar.unwrap_or(0);
    let skip_pager = skip_pager.unwrap_or(0);
    if above == 0 && skip_taskbar == 0 && skip_pager == 0 {
        return false;
    }
    let mut ok = true;
    if above != 0 || skip_taskbar != 0 {
        let event = ClientMessageEvent::new(32, window, wm_state, [1u32, above, skip_taskbar, 1, 0]);
        ok &= conn
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .is_ok();
    }
    if skip_pager != 0 {
        let event = ClientMessageEvent::new(32, window, wm_state, [1u32, skip_pager, 0, 1, 0]);
        ok &= conn
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .is_ok();
    }
    ok
}

/// Lightweight X11 sync dedicated to the `Zalo Notice` popup.
///
/// Runs independently of the main window's `settled` gate and finds the popup
/// via `query_tree` even when a WM omits `_NET_WM_WINDOW_TYPE_NOTIFICATION`
/// windows from `_NET_CLIENT_LIST`.
pub fn sync_notice(scale: f32) {
    if cfg!(test) || std::env::var_os("DISPLAY").is_none() {
        return;
    }
    snap_locked_if_due(scale);
    static CONFIGURED: OnceLock<Mutex<Option<(u32, u16, u16, bool)>>> = OnceLock::new();
    let s = scale.max(1.0);
    let want_w = (crate::notice::NOTICE_W * s).round().clamp(1.0, 4096.0) as u16;
    let want_h = (crate::notice::NOTICE_H * s).round().clamp(1.0, 4096.0) as u16;
    let Some((conn, screen)) = x11rb::connect(None).ok() else {
        return;
    };
    let Some(root) = conn.setup().roots.get(screen).map(|screen| screen.root) else {
        return;
    };
    let Some(window) = find_notice(&conn, root, want_w, want_h) else {
        return;
    };
    let composited = compositor_present(&conn, screen);
    let sig = (window, want_w, want_h, composited);
    let already = CONFIGURED
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .is_some_and(|guard| *guard == Some(sig));
    if already {
        return;
    }
    configure_notice_hints(&conn, root, window, notice_viewable(&conn, window));
    let radius = if composited {
        (4.0 * s).round().clamp(1.0, 32.0) as u16
    } else {
        0
    };
    let _ = write_shape(&conn, window, want_w, want_h, radius, composited);
    let _ = conn.flush();
    if let Ok(mut guard) = CONFIGURED.get_or_init(|| Mutex::new(None)).lock() {
        *guard = Some(sig);
    }
}

/// Current X11 root pointer coordinates `(x, y)` in device pixels.
/// Returns `None` on Wayland, headless tests, or when `DISPLAY` is unset.
pub fn query_pointer_root() -> Option<(i32, i32)> {
    if cfg!(test)
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("DISPLAY").is_none()
    {
        return None;
    }
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let reply = conn.query_pointer(root).ok()?.reply().ok()?;
    Some((i32::from(reply.root_x), i32::from(reply.root_y)))
}

/// True screen origin of the open `Zalo Notice` window in logical pixels.
pub fn notice_root_origin(scale: f32) -> Option<(f32, f32)> {
    if cfg!(test)
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("DISPLAY").is_none()
    {
        return None;
    }
    let s = scale.max(1.0);
    let want_w = (crate::notice::NOTICE_W * s).round().clamp(1.0, 4096.0) as u16;
    let want_h = (crate::notice::NOTICE_H * s).round().clamp(1.0, 4096.0) as u16;
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let window = find_notice(&conn, root, want_w, want_h)?;
    let pos = conn.translate_coordinates(window, root, 0, 0).ok()?.reply().ok()?;
    Some((f32::from(pos.dst_x) / s, f32::from(pos.dst_y) / s))
}

/// Move the open `Zalo Notice` window to `(x, y)` (logical pixels) without
/// unmapping, stealing focus, or handing pointer grab to the WM.
pub fn move_notice_to(x: f32, y: f32, scale: f32) -> bool {
    if cfg!(test)
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("DISPLAY").is_none()
    {
        return false;
    }
    let s = scale.max(1.0);
    let want_x = (x * s).round() as i32;
    let want_y = (y * s).round() as i32;
    let want_w = (crate::notice::NOTICE_W * s).round().clamp(1.0, 4096.0) as u16;
    let want_h = (crate::notice::NOTICE_H * s).round().clamp(1.0, 4096.0) as u16;
    let Some((conn, screen)) = x11rb::connect(None).ok() else {
        return false;
    };
    let Some(root) = conn.setup().roots.get(screen).map(|screen| screen.root) else {
        return false;
    };
    let Some(window) = find_notice(&conn, root, want_w, want_h) else {
        return false;
    };
    move_notice_window(&conn, root, window, want_x, want_y, want_w, want_h, true);
    true
}

/// Push a locked notice toward its corner, at most once every 40 ms.
///
/// No-op when nothing is locked (the user is dragging, or no popup is open).
fn snap_locked_if_due(scale: f32) {
    let Some((lx, ly)) = crate::notice::locked_origin() else {
        return;
    };
    let now = Instant::now();
    let due = match state().try_lock() {
        Ok(mut guard) => {
            let due = guard
                .snapped
                .is_none_or(|at| now.duration_since(at) >= Duration::from_millis(40));
            if due {
                guard.snapped = Some(now);
            }
            due
        }
        Err(_) => false,
    };
    if !due {
        return;
    }
    let s = scale.max(1.0);
    let want_x = (lx * s).round() as i32;
    let want_y = (ly * s).round() as i32;
    let want_w = (crate::notice::NOTICE_W * s).round().clamp(1.0, 4096.0) as u16;
    let want_h = (crate::notice::NOTICE_H * s).round().clamp(1.0, 4096.0) as u16;
    let Some((conn, screen)) = x11rb::connect(None).ok() else {
        return;
    };
    let Some(root) = conn.setup().roots.get(screen).map(|screen| screen.root) else {
        return;
    };
    let Some(window) = find_notice(&conn, root, want_w, want_h) else {
        return;
    };
    if notice_is_at(&conn, window, root, want_x, want_y) {
        return;
    }
    move_notice_window(&conn, root, window, want_x, want_y, want_w, want_h, true);
}

/// Stamp non-focusable notification hints and land the notice on its corner
/// without unmapping/remapping (which would flash and trigger WM focus logic).
pub fn park_notice(scale: f32) {
    if cfg!(test) || std::env::var_os("DISPLAY").is_none() {
        return;
    }
    let Some((lx, ly)) = crate::notice::locked_origin() else {
        return;
    };
    let s = scale.max(1.0);
    let want_x = (lx * s).round() as i32;
    let want_y = (ly * s).round() as i32;
    let want_w = (crate::notice::NOTICE_W * s).round().clamp(1.0, 4096.0) as u16;
    let want_h = (crate::notice::NOTICE_H * s).round().clamp(1.0, 4096.0) as u16;
    let Some((conn, screen)) = x11rb::connect(None).ok() else {
        return;
    };
    let Some(root) = conn.setup().roots.get(screen).map(|screen| screen.root) else {
        return;
    };
    let Some(window) = find_notice(&conn, root, want_w, want_h) else {
        return;
    };
    let viewable = notice_viewable(&conn, window);
    place_notice_window(
        &conn, root, window, want_x, want_y, want_w, want_h, viewable,
    );
    let composited = compositor_present(&conn, screen);
    let radius = if composited {
        (4.0 * s).round().clamp(1.0, 32.0) as u16
    } else {
        0
    };
    let _ = write_shape(&conn, window, want_w, want_h, radius, composited);
    let _ = conn.flush();
}

/// Stamp non-focusable notification hints (`WM_HINTS.input=0`,
/// `_NET_WM_USER_TIME=0`, `USPosition`) before the WM handles `MapRequest`.
///
/// Never unmaps/remaps a mapped window so the WM cannot steal focus or flash.
pub fn anticipate_notice(scale: f32) {
    if cfg!(test) || std::env::var_os("DISPLAY").is_none() {
        return;
    }
    let Some((lx, ly)) = crate::notice::locked_origin() else {
        return;
    };
    let s = scale.max(1.0);
    let want_x = (lx * s).round() as i32;
    let want_y = (ly * s).round() as i32;
    let want_w = (crate::notice::NOTICE_W * s).round().clamp(1.0, 4096.0) as u16;
    let want_h = (crate::notice::NOTICE_H * s).round().clamp(1.0, 4096.0) as u16;
    let _ = std::thread::Builder::new()
        .name("zalo-notice-place".into())
        .spawn(move || {
            let Some((conn, screen)) = x11rb::connect(None).ok() else {
                return;
            };
            let Some(root) = conn.setup().roots.get(screen).map(|screen| screen.root) else {
                return;
            };
            let mut stamped = false;
            for _ in 0..30 {
                if let Some(window) = find_notice(&conn, root, want_w, want_h) {
                    let viewable = notice_viewable(&conn, window);
                    if !viewable {
                        if !stamped {
                            place_notice_window(
                                &conn, root, window, want_x, want_y, want_w, want_h, false,
                            );
                            stamped = true;
                        }
                    } else {
                        if !stamped || !notice_is_at(&conn, window, root, want_x, want_y) {
                            place_notice_window(
                                &conn, root, window, want_x, want_y, want_w, want_h, true,
                            );
                            stamped = true;
                        }
                        if notice_is_at(&conn, window, root, want_x, want_y) {
                            return;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(4));
            }
        });
}

fn notice_viewable(conn: &impl Connection, window: u32) -> bool {
    conn.get_window_attributes(window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|attrs| attrs.map_state == MapState::VIEWABLE)
}

/// Apply non-focusable notification hints and position `(x, y)` without
/// unmapping or remapping.
fn place_notice_window(
    conn: &impl Connection,
    root: u32,
    window: u32,
    x: i32,
    y: i32,
    width: u16,
    height: u16,
    mapped: bool,
) {
    configure_notice_hints(conn, root, window, mapped);
    move_notice_window(conn, root, window, x, y, width, height, mapped);
}

/// Move a notice window via `WM_NORMAL_HINTS` + `ConfigureWindow` (plus
/// `_NET_MOVERESIZE_WINDOW` once mapped) without unmapping or stealing focus.
fn move_notice_window(
    conn: &impl Connection,
    root: u32,
    window: u32,
    x: i32,
    y: i32,
    width: u16,
    height: u16,
    mapped: bool,
) {
    let ww = u32::from(width);
    let hh = u32::from(height);
    // USPosition | USSize | PPosition | PSize | PMinSize | PMaxSize | PWinGravity.
    let flags = 1u32 | 2 | 4 | 8 | 16 | 32 | 512;
    let data = [
        flags, x as u32, y as u32, ww, hh, ww, hh, ww, hh, 0, 0, 0, 0, 0, 0, ww, hh, 1u32,
    ];
    let _ = conn.change_property32(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NORMAL_HINTS,
        AtomEnum::WM_SIZE_HINTS,
        &data,
    );
    let _ = conn.configure_window(
        window,
        &ConfigureWindowAux::new().x(x).y(y).width(ww).height(hh),
    );
    if mapped && !notice_is_at(conn, window, root, x, y) {
        if let Some(moveresize) = conn
            .intern_atom(false, b"_NET_MOVERESIZE_WINDOW")
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.atom)
            .filter(|atom| *atom != 0)
        {
            let move_flags = 1u32 | (1 << 8) | (1 << 9) | (1 << 10) | (1 << 11);
            let event = ClientMessageEvent::new(
                32,
                window,
                moveresize,
                [move_flags, x as u32, y as u32, ww, hh],
            );
            let _ = conn.send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            );
        }
    }
    let _ = conn.flush();
}

fn notice_is_at(conn: &impl Connection, window: u32, root: u32, want_x: i32, want_y: i32) -> bool {
    let Ok(cookie) = conn.translate_coordinates(window, root, 0, 0) else {
        return false;
    };
    let Ok(pos) = cookie.reply() else {
        return false;
    };
    (i32::from(pos.dst_x) - want_x).abs() <= 2 && (i32::from(pos.dst_y) - want_y).abs() <= 2
}

/// Our notice client: titled `Zalo Notice`, or the only 352×82 window we own.
fn find_notice(conn: &impl Connection, root: u32, width: u16, height: u16) -> Option<u32> {
    let wm_pid = conn
        .intern_atom(false, b"_NET_WM_PID")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let wm_name = conn
        .intern_atom(false, b"_NET_WM_NAME")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let utf8 = conn
        .intern_atom(false, b"UTF8_STRING")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let pid = std::process::id();
    let mut sized = None;
    for window in owned_clients(conn, root, wm_pid, pid) {
        let name = window_name(conn, window, wm_name, utf8);
        if name.starts_with("Zalo Notice") {
            return Some(window);
        }
        let Ok(cookie) = conn.get_geometry(window) else {
            continue;
        };
        let Ok(geo) = cookie.reply() else {
            continue;
        };
        if geo.width == width && geo.height == height {
            sized = Some(window);
        }
    }
    sized
}

fn owned_clients(conn: &impl Connection, root: u32, wm_pid: u32, pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let Some(top) = conn
        .query_tree(root)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return out;
    };
    for child in top.children {
        if prop32(conn, child, wm_pid).is_some_and(|values| values.first() == Some(&pid)) {
            out.push(child);
            continue;
        }
        let Some(frame) = conn
            .query_tree(child)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            continue;
        };
        for client in frame.children {
            if prop32(conn, client, wm_pid).is_some_and(|values| values.first() == Some(&pid)) {
                out.push(client);
            }
        }
    }
    out
}

/// `_NET_WM_ICON` at 16 and 22 px. xfwm draws a missing property from the
/// 256 px theme icon, and with the compositor off that icon is clipped.
fn pin_call_icon(conn: &impl Connection, window: u32) {
    static DONE: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    let done = DONE.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(set) = done.lock() else {
        return;
    };
    if set.contains(&window) {
        return;
    }
    drop(set);
    if !write_call_icon(conn, window) {
        return;
    }
    if let Ok(mut set) = done.lock() {
        set.insert(window);
    }
}

fn write_call_icon(conn: &impl Connection, window: u32) -> bool {
    let Some(data) = call_icon_cardinals() else {
        return false;
    };
    let Ok(cookie) = conn.intern_atom(false, b"_NET_WM_ICON") else {
        return false;
    };
    let Ok(atom) = cookie.reply() else {
        return false;
    };
    if atom.atom == 0 {
        return false;
    }
    let Ok(cookie) = conn.change_property32(
        PropMode::REPLACE,
        window,
        atom.atom,
        AtomEnum::CARDINAL,
        data,
    ) else {
        return false;
    };
    let _ = cookie.check();
    let _ = conn.flush();
    true
}

fn call_icon_cardinals() -> Option<&'static [u32]> {
    static DATA: OnceLock<Vec<u32>> = OnceLock::new();
    let data = DATA.get_or_init(load_call_icon);
    (!data.is_empty()).then_some(data.as_slice())
}

fn load_call_icon() -> Vec<u32> {
    let mut out = Vec::new();
    for size in ["16x16", "22x22"] {
        let Some(part) = png_cardinals(&icon_png(size)) else {
            continue;
        };
        out.extend(part);
    }
    out
}

fn icon_png(size: &str) -> std::path::PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/share"));
    base.join(format!("icons/hicolor/{size}/apps/zalo-gpui.png"))
}

fn png_cardinals(path: &std::path::Path) -> Option<Vec<u32>> {
    let img = image::open(path).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || w > 48 || h > 48 {
        return None;
    }
    let mut data = Vec::with_capacity(2 + (w * h) as usize);
    data.push(w);
    data.push(h);
    for px in img.pixels() {
        let [r, g, b, a] = px.0;
        data.push((u32::from(a) << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b));
    }
    Some(data)
}

fn prop32(conn: &impl Connection, window: u32, property: u32) -> Option<Vec<u32>> {
    let reply = conn
        .get_property(false, window, property, 0u32, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    let values = reply.value32()?;
    Some(values.collect())
}

fn window_name(conn: &impl Connection, window: u32, net_wm_name: u32, utf8: u32) -> String {
    if let Some(name) = text_prop(conn, window, net_wm_name, utf8) {
        return name;
    }
    text_prop(
        conn,
        window,
        AtomEnum::WM_NAME.into(),
        AtomEnum::STRING.into(),
    )
    .unwrap_or_default()
}

fn text_prop(conn: &impl Connection, window: u32, property: u32, type_: u32) -> Option<String> {
    let reply = conn
        .get_property(false, window, property, type_, 0, 256)
        .ok()?
        .reply()
        .ok()?;
    let name = String::from_utf8_lossy(&reply.value);
    let name = name.trim_end_matches('\0').trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_rows_cuts_the_corners_and_keeps_the_middle() {
        let rows = rounded_rows(100, 80, 10);
        let middle = rows.iter().find(|row| row.height > 1).expect("middle band");
        assert_eq!(middle.y, 10);
        assert_eq!(middle.height, 60);
        assert_eq!(middle.width, 100);
        assert_eq!(middle.x, 0);
        let top = rows.iter().find(|row| row.y == 0).expect("top row");
        assert!(top.x > 0, "the top row is inset");
        assert!(top.width < 100);
        let bottom = rows.iter().find(|row| row.y == 79).expect("bottom row");
        assert_eq!(bottom.width, top.width);
        assert_eq!(bottom.x, top.x);
        assert!(rows.iter().any(|row| row.y == 9 && row.width > top.width));
    }

    #[test]
    fn cover_keeps_the_fringe_the_tight_mask_cuts() {
        let tight = rounded_rows(200, 160, 12);
        let cover = rounded_rows_cover(200, 160, 12);
        let top = tight.iter().find(|row| row.y == 0).expect("tight top");
        let cover_top = cover.iter().find(|row| row.y == 0).expect("cover top");
        assert!(cover_top.x < top.x, "the cover starts further out");
        assert!(cover_top.width > top.width);
        assert_eq!(cover_top.x + cover_top.width as i16, (200 - cover_top.x));
    }

    #[test]
    fn shape_masks_never_apply_1bit_bounding_staircase() {
        let (b_comp, i_comp) = shape_masks(800, 600, 12, true);
        assert_eq!(b_comp.len(), 1, "composited bounding stays full rect for smooth ARGB AA");
        assert_eq!(b_comp[0].width, 800);
        assert_eq!(b_comp[0].height, 600);
        assert!(i_comp.len() > 1, "composited input mask still clips rounded corners");

        let (b_plain, i_plain) = shape_masks(800, 600, 12, false);
        assert_eq!(b_plain.len(), 1, "non-composited bounding stays crisp square");
        assert_eq!(i_plain.len(), 1, "non-composited input stays crisp square");
    }

    #[test]
    fn square_when_the_radius_does_not_fit() {
        assert_eq!(rounded_rows(40, 30, 0).len(), 1);
        assert_eq!(rounded_rows(20, 20, 12).len(), 1);
        let full = &rounded_rows(40, 30, 0)[0];
        assert_eq!((full.width, full.height), (40, 30));
    }

    #[test]
    fn sync_does_not_touch_the_display_under_test() {
        sync(1.0);
        sync_notice(1.0);
        park_notice(1.0);
        anticipate_notice(1.0);
        assert_eq!(query_pointer_root(), None);
        assert_eq!(notice_root_origin(1.0), None);
        assert!(!move_notice_to(10.0, 20.0, 1.0));
    }
}
