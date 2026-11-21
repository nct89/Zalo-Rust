//! Native GPUI shell for the Zalo Linux Rust client.
//!
//! Toolkit: GPUI 0.2.2 (Zed, Discord Phoenix column). Compact-app React is not
//! translated; layout comes from `zalo-design` CSS constants and `STR_*`.
//! Protocol crates keep `unsafe_code = forbid`. This crate allows unsafe
//! because GPUI macros may emit GPU/window FFI.

#![allow(unsafe_code)]

mod account;
mod audio_play;
mod autoreply;
mod background;
mod board;
mod bridge;
mod call;
mod call_camera;
mod call_screen;
mod call_video;
mod chat;
mod clip;
mod color;
mod contacts;
mod context_menu;
mod data;
mod dialogs;
mod emoji;
mod fetch;
mod fonts;
mod i18n;
mod info;
mod installer;
mod labels;
mod line_input;
mod links;
mod login;
mod login_flow;
mod media;
mod media_store;
mod notice;
mod pc_export;
mod photo_window;
mod plugins;
mod prefs;
mod presence;
mod preview;
mod privacy;
mod profiles;
mod qr;
mod quick;
mod rail;
mod read_sync;
mod realtime;
mod calendar;
mod screenshot_ui;
mod settings;
mod shell;
mod shortcuts;
mod sidebar;
mod stickers;
mod titlebar;
mod video;
mod viewer;
mod voice;
mod widgets;
mod window_frame;
mod x11_frame;

pub use call::restore_saved_mic_volume;
pub use chat::COMPOSER_INPUT_H;
pub use color::{parse_hex, str_vi, token_rgb};
pub use data::session_present;
pub use fonts::{
    install as install_fonts, report as font_report, FontReport, FAMILY_BOLD, FAMILY_ICON,
    FAMILY_SEMIBOLD, FAMILY_TEXT, FAMILY_UI,
};
pub use installer::{scan_system, DistroFamily, InstallerView, SystemScan};
pub use line_input::bind_keys as bind_line_keys;
pub use login::LoginView;
pub use preview::{default_list_tabs, DEFAULT_SPLIT};
pub use qr::{rasterize_qr, PREVIEW_QR_PAYLOAD, QR_TIMEOUT_SECS};
pub use rail::{lists_chats, Rail, RAIL_MARK_W};
pub use settings::{SettingsPage, SETTING_MENU_W, SETTING_MODAL_W};
pub use shell::{
    main_window_min, main_window_size, preview_shell_requested, ShellView, MAIN_WINDOW_DEFAULT,
    MAIN_WINDOW_MIN_H,
};
pub use stickers::{StickerTab, PANEL_H as STICKER_PANEL_H, PANEL_W as STICKER_PANEL_W};
pub use window_frame::outer_size as window_outer_size;

/// Caption button width on the Linux `#titleBar` (Zalo wrapper).
pub const CAPTION_BTN_W: f32 = 46.0;

/// Open the login window at the CSS size `LOGIN_WINDOW` (420×644).
pub fn login_window_size() -> (f32, f32) {
    zalo_design::LOGIN_WINDOW
}

use gpui::{
    point, px, size, App, AppContext, Bounds, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowOptions,
};

/// Close every shell window then open the 420×644 QR login window alone.
///
/// The legacy Electron client keeps one window and swaps the
/// renderer to the login page. GPUI has two view types, so the equivalent
/// is: login window only, no chat chrome behind it.
pub fn replace_shell_with_login(cx: &mut App) {
    open_login_window(cx);
    close_all_shell_windows(cx);
    // If called from inside an active `ShellView` window callback, `handle.update`
    // cannot borrow that window synchronously; defer one tick to close any straggler.
    cx.defer(close_all_shell_windows);
}

/// Closes all open non-login windows (main shell, solo chat, photo viewer,
/// call/incoming windows, notices, screenshot overlays) and activates the login window.
pub fn close_all_shell_windows(cx: &mut App) {
    let to_close: Vec<_> = cx
        .windows()
        .into_iter()
        .filter(|w| w.downcast::<LoginView>().is_none())
        .collect();
    for handle in to_close {
        let _ = handle.update(cx, |_, window, _| {
            window.remove_window();
        });
    }
    for w in cx.windows() {
        if let Some(login) = w.downcast::<LoginView>() {
            let _ = login.update(cx, |_, window, _| window.activate_window());
        }
    }
}

/// Open the 420×644 QR login window (or focus it if already open).
pub fn open_login_window(cx: &mut App) {
    if let Some(existing) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<LoginView>())
    {
        let _ = existing.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let (width, height) = crate::login_window_size();
    let outer = crate::window_frame::outer_size(width, height);
    let bounds = bounds_in_work_area(outer);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            is_resizable: false,
            is_minimizable: true,
            window_decorations: Some(WindowDecorations::Client),
            window_background: WindowBackgroundAppearance::Transparent,
            window_min_size: Some(outer),
            app_id: Some("zalo-gpui".into()),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| LoginView::new(window, cx)),
    )
    .expect("open login window");
}

/// System tray: start StatusNotifierItem (with automatic watcher reconnect +
/// optional X11 XEmbed fallback) and single-instance IPC for Dock quicklists.
pub fn start_tray(cx: &mut App) {
    if std::env::var_os("ZALO_GPUI_NO_TRAY").is_some() {
        return;
    }
    let _ = zalo_shell::install_gpui_app_entry();
    zalo_shell::install_desktop_shell_for(zalo_shell::GPUI_APP_ID);
    zalo_shell::start_instance_listener(zalo_shell::GPUI_APP_ID);
    let initial_muted = crate::prefs::Prefs::load(data::config_dir()).bool("notifyOff", false);
    zalo_shell::set_tray_muted(initial_muted);
    cx.spawn(async move |cx| loop {
        gpui::Timer::after(std::time::Duration::from_millis(250)).await;
        let events = zalo_shell::take_tray_events();
        if events.is_empty() {
            continue;
        }
        if events.contains(&zalo_shell::TrayEvent::Quit) {
            zalo_shell::stop_instance_listener(zalo_shell::GPUI_APP_ID);
            let _ = cx.update(|cx| cx.quit());
            break;
        }
        for event in events {
            let res = match event {
                zalo_shell::TrayEvent::Activate | zalo_shell::TrayEvent::SecondaryActivate => cx
                    .update(|cx| {
                        show_main_window(cx);
                        cx.activate(true);
                    }),
                zalo_shell::TrayEvent::Hide => cx.update(hide_main_window),
                zalo_shell::TrayEvent::ContextMenu => cx.update(open_tray_menu),
                zalo_shell::TrayEvent::ToggleMute => cx.update(toggle_notify_mute),
                zalo_shell::TrayEvent::Quit => Ok(()),
            };
            if res.is_err() {
                return;
            }
        }
    })
    .detach();
}

/// Toggle `notifyOff` from tray/Quicklist and refresh any open shell window.
fn toggle_notify_mute(cx: &mut App) {
    let mut prefs = crate::prefs::Prefs::load(data::config_dir());
    let next = !prefs.bool("notifyOff", false);
    prefs.set("notifyOff", serde_json::Value::Bool(next));
    zalo_shell::set_tray_muted(next);
    for w in cx.windows() {
        if let Some(shell) = w.downcast::<ShellView>() {
            let _ = shell.update(cx, |view, _window, cx| {
                view.sync_notify_pref_from_disk();
                view.set_note(if next {
                    "Đã tắt thông báo tin nhắn"
                } else {
                    "Đã bật thông báo tin nhắn"
                });
                cx.notify();
            });
        }
    }
}

/// Open the main window and show the in-app tray context menu (fallback when a
/// tray host calls SNI `ContextMenu` directly or on XEmbed right-click).
pub fn open_tray_menu(cx: &mut App) {
    show_main_window(cx);
    cx.activate(true);
    let main = cx
        .windows()
        .into_iter()
        .filter_map(|w| w.downcast::<ShellView>())
        .find(|w| w.read(cx).is_ok_and(|v| v.solo.is_none()));
    if let Some(w) = main {
        let _ = w.update(cx, |view, window, cx| {
            view.open_tray_context_menu(window);
            cx.notify();
        });
    }
}

/// Bring the main shell (or active login) window forward, reopening from the tray.
pub fn show_main_window(cx: &mut App) {
    let main = cx
        .windows()
        .into_iter()
        .filter_map(|w| w.downcast::<ShellView>())
        .find(|w| w.read(cx).is_ok_and(|v| v.solo.is_none()));
    if let Some(w) = main {
        let _ = w.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let login = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<LoginView>());
    if let Some(w) = login {
        let _ = w.update(cx, |_, window, _| window.activate_window());
        return;
    }
    if shell::preview_shell_requested() {
        open_shell_window(cx);
    } else {
        open_login_window(cx);
    }
}

/// Hide/minimize the main window to the system tray (matching native "Hide Zalo").
pub fn hide_main_window(cx: &mut App) {
    for w in cx.windows() {
        if let Some(shell) = w.downcast::<ShellView>() {
            if shell.read(cx).is_ok_and(|v| v.solo.is_none()) {
                let _ = shell.update(cx, |_, window, _| window.minimize_window());
            }
        } else if let Some(login) = w.downcast::<LoginView>() {
            let _ = login.update(cx, |_, window, _| window.minimize_window());
        }
    }
    background::tell_tray_once();
}

/// `--hidden` (autostart): no window, listener + tray only. False when
/// there is no tray to come back from, or no session.
pub fn start_hidden(cx: &mut App) -> bool {
    let _ = cx;
    if !data::session_present() {
        return false;
    }
    if !zalo_shell::wait_for_tray_ready(std::time::Duration::from_millis(750)) {
        return false;
    }
    match data::active_data_dir() {
        Some(dir) => {
            background::ensure_started(dir);
            true
        }
        None => false,
    }
}

/// Window closed: keep running in the tray, or quit when nothing is left.
pub fn on_last_window_closed(cx: &mut App) {
    if !cx.windows().is_empty() {
        return;
    }
    if zalo_shell::tray_active() {
        background::tell_tray_once();
    } else {
        let prefs_dir = data::config_dir().filter(|d| d.is_dir());
        let p = prefs::Prefs::load(prefs_dir);
        if !p.bool("rememberLogin", true) {
            if let Some(d) = data::active_data_dir() {
                data::detach_account(&d);
            }
        }
        zalo_shell::stop_instance_listener(zalo_shell::GPUI_APP_ID);
        cx.quit();
    }
}

/// Last shell size from `gpui-prefs.json` `window` (`w`/`h` outer px, `max`).
fn saved_shell_bounds(_cx: &App) -> (gpui::Size<gpui::Pixels>, bool) {
    let (def_w, def_h) = crate::main_window_size();
    let (min_w, min_h) = crate::main_window_min();
    let default = crate::window_frame::outer_size(def_w, def_h);
    let prefs = prefs::Prefs::load(data::config_dir());
    let Some(v) = prefs.json("window") else {
        return (default, false);
    };
    let w = v
        .get("w")
        .and_then(serde_json::Value::as_f64)
        .map(|n| n as f32)
        .unwrap_or_else(|| f32::from(default.width));
    let h = v
        .get("h")
        .and_then(serde_json::Value::as_f64)
        .map(|n| n as f32)
        .unwrap_or_else(|| f32::from(default.height));
    let max = v
        .get("max")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let outer_min = crate::window_frame::outer_size(min_w, min_h);
    let size = size(
        px(w.clamp(f32::from(outer_min.width), 4096.0)),
        px(h.clamp(f32::from(outer_min.height), 4096.0)),
    );
    (size, max)
}

/// Center `size` in the first `_NET_WORKAREA` rectangle (the primary
/// monitor minus the panel). Falls back to the top-left if that rectangle
/// is missing.
fn bounds_in_work_area(size: gpui::Size<gpui::Pixels>) -> Bounds<gpui::Pixels> {
    let (wx, wy, ww, wh) = crate::notice::work_area(1.0, (0.0, 0.0, 1920.0, 1080.0));
    let w = f32::from(size.width);
    let h = f32::from(size.height);
    let x = if w < ww { wx + (ww - w) * 0.5 } else { wx };
    let y = if h < wh { wy + (wh - h) * 0.5 } else { wy };
    Bounds {
        origin: point(px(x), px(y)),
        size,
    }
}

/// Open the main shell window.
///
/// A centered bounds uses GPUI's primary display, which on X11 is the whole
/// virtual screen. With monitors stacked vertically that lands on the seam
/// and xfwm never maps the window. Park a restored window inside
/// `_NET_WORKAREA` instead.
pub fn open_shell_window(cx: &mut App) {
    let (min_w, min_h) = crate::main_window_min();
    let (size, maximized) = saved_shell_bounds(cx);
    let bounds = if maximized {
        Bounds::centered(None, size, cx)
    } else {
        bounds_in_work_area(size)
    };
    let window_bounds = if maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: None,
            is_resizable: true,
            is_minimizable: true,
            window_decorations: Some(WindowDecorations::Client),
            window_background: WindowBackgroundAppearance::Transparent,
            window_min_size: Some(crate::window_frame::outer_size(min_w, min_h)),
            app_id: Some("zalo-gpui".into()),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| ShellView::new(window, cx)),
    )
    .expect("open shell window");
}

/// Open the native Linux Distro Scanner & OCR Installer window (`--install`).
pub fn open_installer_window(cx: &mut App) {
    let outer = crate::window_frame::outer_size(440.0, 214.0);
    let bounds = bounds_in_work_area(outer);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            is_resizable: false,
            is_minimizable: true,
            window_decorations: Some(WindowDecorations::Client),
            window_background: WindowBackgroundAppearance::Transparent,
            window_min_size: Some(outer),
            app_id: Some("zalo-gpui".into()),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| InstallerView::new(window, cx)),
    )
    .expect("open installer window");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_window_matches_css() {
        assert_eq!(login_window_size(), (420.0, 644.0));
        assert_eq!(zalo_design::LINUX_TITLEBAR_H, 38.0);
        assert_eq!(zalo_design::LOGIN_QR_FRAME, 250.0);
        assert_eq!(zalo_design::LOGIN_QR_IMAGE, 240.0);
        assert_eq!(CAPTION_BTN_W, 46.0);
    }

    #[test]
    fn shell_layout_matches_css() {
        assert_eq!(
            zalo_design::MAIN_TAB_W + zalo_design::CONV_LIST_INNER_W,
            zalo_design::SIDEBAR_W
        );
        assert_eq!(zalo_design::SIDEBAR_W, 408.0);
        assert_eq!(zalo_design::MAIN_TAB_W, 64.0);
        assert_eq!(zalo_design::CONV_LIST_INNER_W, 344.0);
        assert_eq!(zalo_design::LINUX_TITLEBAR_H, 38.0);
        assert_eq!(zalo_design::CONV_ITEM_H, 78.0);
        assert_eq!(zalo_design::CHAT_HEADER_H, 68.0);
        assert_eq!(RAIL_MARK_W, 48.0);
        assert_eq!(zalo_design::LEFTBAR_ICON_PX, 28.0);
        assert_eq!(zalo_design::LEFTBAR_MARK_RADIUS, 6.0);
        assert_eq!(zalo_design::SEARCH_ACTION_ICON, 22.0);
        assert_eq!(zalo_design::SEARCH_INPUT_RADIUS, 5.0);
        assert_eq!(zalo_design::CARD_RADIUS, 8.0);
        assert_eq!(COMPOSER_INPUT_H, 51.0);
        assert_eq!(main_window_min(), (580.0, 400.0));
        assert_eq!(MAIN_WINDOW_DEFAULT, (1200.0, 720.0));
    }

    #[test]
    fn default_tabs_are_all_and_unread() {
        assert!(!DEFAULT_SPLIT);
        let (first, second) = default_list_tabs();
        assert_eq!(first, "Tất cả");
        assert_eq!(second, "Chưa đọc");
        assert_eq!(str_vi("STR_TAB_CONV_FOCUS", ""), "Ưu tiên");
        assert_eq!(str_vi("STR_TAB_CONV_OTHER", ""), "Khác");
        assert!(lists_chats(Rail::Chats));
        assert!(!lists_chats(Rail::Contacts));
        assert_eq!(SETTING_MENU_W, 228.0);
        assert_eq!(STICKER_PANEL_W, 360.0);
        assert_eq!(STICKER_PANEL_H, 420.0);
        assert_eq!(SettingsPage::ALL.len(), 11);
        assert_eq!(StickerTab::Sticker as u8, 0);
        assert_eq!(zalo_design::WINDOW_RADIUS, 8.0);
        assert_eq!(zalo_design::CONTEXT_MENU_RADIUS, 4.0);
        assert!(crate::context_menu::conv_menu_labels()
            .iter()
            .any(|l| l == "Ghim hội thoại"));
    }
}
