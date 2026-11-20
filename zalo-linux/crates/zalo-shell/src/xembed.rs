//! Pure-Rust X11 XEmbed system tray fallback (`_NET_SYSTEM_TRAY_S<screen>`).
//!
//! Used only when built with `--features tray-xembed` and no StatusNotifier
//! watcher (`org.kde.StatusNotifierWatcher`) owns the tray (for example
//! i3bar, polybar, tint2, Openbox, dwm, or legacy X11 Notification Area).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ColormapAlloc, ConnectionExt as _, CreateGCAux, CreateWindowAux,
    EventMask, ImageFormat, PropMode, VisualClass, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::badge::{render_tray_rgba, rgba_to_x11_zpixmap};
use crate::tray::{record_tray_event, TrayEvent};

const SYSTEM_TRAY_REQUEST_DOCK: u32 = 0;
const XEMBED_MAPPED: u32 = 1 << 0;
const DEFAULT_TRAY_PX: u16 = 24;
const FALLBACK_BG_RGB: [u8; 3] = [43, 45, 49];

pub(crate) struct XEmbedControl {
    stop: AtomicBool,
    dirty: AtomicBool,
    count: AtomicU32,
}

impl XEmbedControl {
    pub(crate) fn update_count(&self, count: u32) {
        self.count.store(count, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Release);
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// True when `$DISPLAY` has an active X11 `_NET_SYSTEM_TRAY_S<screen>` owner.
pub(crate) fn xembed_tray_present() -> bool {
    let Ok((conn, screen_num)) = RustConnection::connect(None) else {
        return false;
    };
    let atom_name = format!("_NET_SYSTEM_TRAY_S{screen_num}");
    let Ok(cookie) = conn.intern_atom(false, atom_name.as_bytes()) else {
        return false;
    };
    let Ok(reply) = cookie.reply() else {
        return false;
    };
    conn.get_selection_owner(reply.atom)
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|r| r.owner != x11rb::NONE)
}

/// Spawn the XEmbed tray worker on `DISPLAY`. Returns `None` if X11 or the
/// selection owner is unavailable.
pub(crate) fn start_xembed_tray(
    base_png: Vec<u8>,
    initial_count: u32,
    on_active_change: Arc<dyn Fn(bool) + Send + Sync>,
) -> Option<Arc<XEmbedControl>> {
    if !xembed_tray_present() {
        return None;
    }
    let control = Arc::new(XEmbedControl {
        stop: AtomicBool::new(false),
        dirty: AtomicBool::new(true),
        count: AtomicU32::new(initial_count),
    });
    let worker_control = Arc::clone(&control);
    std::thread::Builder::new()
        .name("zalo-tray-xembed".into())
        .spawn(move || {
            run_xembed(base_png, worker_control, on_active_change);
        })
        .ok()?;
    Some(control)
}

struct TrayAtoms {
    opcode: u32,
    visual_prop: u32,
    xembed_info: u32,
    net_wm_name: u32,
    utf8_string: u32,
    visualid_type: u32,
}

fn intern(conn: &RustConnection, name: &[u8]) -> Option<u32> {
    conn.intern_atom(false, name)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.atom)
}

fn find_argb_visual(
    conn: &RustConnection,
    screen_num: usize,
    tray_owner: u32,
    atoms: &TrayAtoms,
) -> Option<u32> {
    let screen = &conn.setup().roots[screen_num];
    let preferred = conn
        .get_property(
            false,
            tray_owner,
            atoms.visual_prop,
            atoms.visualid_type,
            0,
            1,
        )
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().and_then(|mut it| it.next()));

    let mut fallback_32 = None;
    for depth_info in &screen.allowed_depths {
        if depth_info.depth != 32 {
            continue;
        }
        for vis in &depth_info.visuals {
            if vis.class != VisualClass::TRUE_COLOR {
                continue;
            }
            if preferred == Some(vis.visual_id) {
                return Some(vis.visual_id);
            }
            if fallback_32.is_none() {
                fallback_32 = Some(vis.visual_id);
            }
        }
    }
    if preferred.is_some() {
        // Tray specifically advertises a 32-bit visual; only fall back if one exists.
        return fallback_32;
    }
    None
}

fn run_xembed(
    base_png: Vec<u8>,
    control: Arc<XEmbedControl>,
    on_active_change: Arc<dyn Fn(bool) + Send + Sync>,
) {
    let Ok((conn, screen_num)) = RustConnection::connect(None) else {
        return;
    };
    let sel_name = format!("_NET_SYSTEM_TRAY_S{screen_num}");
    let Some(sel_atom) = intern(&conn, sel_name.as_bytes()) else {
        return;
    };
    let Some(owner_reply) = conn
        .get_selection_owner(sel_atom)
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return;
    };
    let tray_owner = owner_reply.owner;
    if tray_owner == x11rb::NONE {
        return;
    }

    let Some(atoms) = (|| {
        Some(TrayAtoms {
            opcode: intern(&conn, b"_NET_SYSTEM_TRAY_OPCODE")?,
            visual_prop: intern(&conn, b"_NET_SYSTEM_TRAY_VISUAL")?,
            xembed_info: intern(&conn, b"_XEMBED_INFO")?,
            net_wm_name: intern(&conn, b"_NET_WM_NAME")?,
            utf8_string: intern(&conn, b"UTF8_STRING")?,
            visualid_type: u32::from(AtomEnum::VISUALID),
        })
    })() else {
        return;
    };

    let root = conn.setup().roots[screen_num].root;
    let argb_visual = find_argb_visual(&conn, screen_num, tray_owner, &atoms);
    let (depth, visual, colormap) = if let Some(vid) = argb_visual {
        let Ok(cmap) = conn.generate_id() else {
            return;
        };
        if conn
            .create_colormap(ColormapAlloc::NONE, cmap, root, vid)
            .is_err()
        {
            return;
        }
        (32u8, vid, Some(cmap))
    } else {
        (0u8, x11rb::COPY_FROM_PARENT, None)
    };
    let argb32 = argb_visual.is_some();

    let Ok(win) = conn.generate_id() else {
        return;
    };
    let mut aux = CreateWindowAux::new()
        .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS | EventMask::STRUCTURE_NOTIFY)
        .border_pixel(0);
    if let Some(cmap) = colormap {
        aux = aux.colormap(cmap).background_pixel(0);
    }

    let mut width = DEFAULT_TRAY_PX;
    let mut height = DEFAULT_TRAY_PX;

    if conn
        .create_window(
            depth,
            win,
            root,
            0,
            0,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &aux,
        )
        .ok()
        .and_then(|c| c.check().ok())
        .is_none()
    {
        return;
    }

    let _ = conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"zalo-tray\0Zalo\0",
    );
    let _ = conn.change_property8(
        PropMode::REPLACE,
        win,
        atoms.net_wm_name,
        atoms.utf8_string,
        b"Zalo",
    );
    let _ = conn.change_property32(
        PropMode::REPLACE,
        win,
        atoms.xembed_info,
        atoms.xembed_info,
        &[0, XEMBED_MAPPED],
    );

    let Ok(gc) = conn.generate_id() else {
        let _ = conn.destroy_window(win);
        return;
    };
    if conn
        .create_gc(gc, win, &CreateGCAux::new())
        .ok()
        .and_then(|c| c.check().ok())
        .is_none()
    {
        let _ = conn.destroy_window(win);
        return;
    }

    let dock_msg = ClientMessageEvent::new(
        32,
        tray_owner,
        atoms.opcode,
        [x11rb::CURRENT_TIME, SYSTEM_TRAY_REQUEST_DOCK, win, 0, 0],
    );
    if conn
        .send_event(false, tray_owner, EventMask::NO_EVENT, dock_msg)
        .ok()
        .and_then(|c| c.check().ok())
        .is_none()
    {
        let _ = conn.destroy_window(win);
        return;
    }
    let _ = conn.flush();

    on_active_change(true);
    let put_depth = if argb32 {
        32
    } else {
        conn.setup().roots[screen_num].root_depth
    };

    let mut ticks = 0u32;
    while !control.stop.load(Ordering::Acquire) {
        ticks = ticks.wrapping_add(1);
        if ticks % 25 == 0 {
            let live_owner = conn
                .get_selection_owner(sel_atom)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.owner)
                .unwrap_or(x11rb::NONE);
            if live_owner == x11rb::NONE {
                control.stop.store(true, Ordering::Release);
                break;
            }
        }
        loop {
            match conn.poll_for_event() {
                Ok(Some(Event::Expose(ev))) if ev.count == 0 => {
                    control.dirty.store(true, Ordering::Release);
                }
                Ok(Some(Event::ConfigureNotify(ev))) => {
                    let next_w = ev.width.max(8);
                    let next_h = ev.height.max(8);
                    if next_w != width || next_h != height {
                        width = next_w;
                        height = next_h;
                        control.dirty.store(true, Ordering::Release);
                    }
                }
                Ok(Some(Event::ButtonPress(ev))) => match ev.detail {
                    1 => record_tray_event(TrayEvent::Activate),
                    2 => record_tray_event(TrayEvent::SecondaryActivate),
                    3 => record_tray_event(TrayEvent::ContextMenu),
                    _ => {}
                },
                Ok(Some(Event::DestroyNotify(_))) => {
                    control.stop.store(true, Ordering::Release);
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    control.stop.store(true, Ordering::Release);
                    break;
                }
            }
        }
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        if control.dirty.swap(false, Ordering::AcqRel) {
            let count = control.count.load(Ordering::Relaxed);
            if let Ok(rgba) = render_tray_rgba(&base_png, count) {
                let bytes = rgba_to_x11_zpixmap(&rgba, width, height, argb32, FALLBACK_BG_RGB);
                let _ = conn.put_image(
                    ImageFormat::Z_PIXMAP,
                    win,
                    gc,
                    width,
                    height,
                    0,
                    0,
                    0,
                    put_depth,
                    &bytes,
                );
                let _ = conn.flush();
            }
        }
        std::thread::sleep(Duration::from_millis(80));
    }

    on_active_change(false);
    let _ = conn.free_gc(gc);
    let _ = conn.destroy_window(win);
    if let Some(cmap) = colormap {
        let _ = conn.free_colormap(cmap);
    }
    let _ = conn.flush();
}
