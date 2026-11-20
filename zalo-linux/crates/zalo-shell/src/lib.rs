//! Tray icon, Unity launcher badge, desktop notifications, launch on login,
//! and the system light/dark preference.
//!
//! The behaviour follows `plugins/tray-badge`, `plugins/launcher-badge`, and
//! `plugins/tray-host`. [`install_desktop_shell`] registers a StatusNotifier
//! item when a watcher owns `org.kde.StatusNotifierWatcher`. It does not send
//! a notification. [`publish_unread`] is the call that emits the Unity signal
//! and, for a positive count, `org.freedesktop.Notifications.Notify`.
//!
//! Unit tests build payloads and icons only. They do not open the session bus.

#![forbid(unsafe_code)]

mod appearance;
mod appentry;
mod autostart;
mod badge;
mod bus;
mod error;
mod filechooser;
mod instance;
mod launcher;
mod notify;
mod portal;
mod screenshot;
mod shortcuts;
mod tray;
mod window_controls;
#[cfg(feature = "tray-xembed")]
mod xembed;

pub use appearance::{
    gsettings_scheme_is_dark, gtk_theme_is_dark, scheme_is_dark, system_prefers_dark,
    PORTAL_SETTINGS_INTERFACE,
};
pub use appentry::{
    app_desktop_entry, data_home, install_app_entry, install_app_entry_at, install_gpui_app_entry,
    APP_ID, GPUI_APP_ID, ICON_SIZES,
};
pub use autostart::{
    autostart_enabled, autostart_enabled_at, autostart_path, desktop_entry, desktop_entry_for,
    set_autostart, set_autostart_at, set_autostart_for, set_autostart_for_at, AUTOSTART_FILE,
};
pub use badge::{
    badge_label, default_base_png, normalize_count, normalize_count_text, render_tray_badge,
    zalo_icon_png, zalo_icon_rgba, BASE_ICON_PX, ZALO_ICON_PNG,
};
pub use bus::{
    watcher_present, NOTIFICATIONS_PATH, NOTIFICATIONS_SERVICE, STATUS_NOTIFIER_WATCHER,
};
pub use error::ShellError;
pub use filechooser::{
    parse_dialog_output, pick_file, pick_folder, pick_image, zenity_filter, IMAGE_PATTERNS,
    PORTAL_FILECHOOSER_INTERFACE,
};
pub use instance::{
    instance_command_from_args, instance_socket_path, ping_instance, profile_arg_from_args,
    start_instance_listener, stop_instance_listener, try_send_instance_command, InstanceCommand,
};
pub use launcher::{
    desktop_files, desktop_files_from_env, normalize_desktop_file, parse_title_count,
    publish_unity_badge, unity_badge_payload, DesktopQuery, UNITY_INTERFACE, UNITY_OBJECT_PATH,
    UNITY_SIGNAL,
};
pub use notify::{notify_call, notify_desktop, unread_notice, DesktopNotice, NotifyCall};
pub use portal::{
    current_desktop, desktop_shell_status, detect_desktop_from, format_shell_hint, is_flatpak,
    request_background, request_background_async, BackgroundGrant, DesktopEnvironment,
    DesktopShellStatus, PORTAL_BACKGROUND_INTERFACE,
};
pub use screenshot::{
    capture_fullscreen, capture_interactive, capture_screenshot, parse_file_uri,
    portal_request_path, PORTAL_PATH, PORTAL_REQUEST_INTERFACE, PORTAL_SCREENSHOT_INTERFACE,
    PORTAL_SERVICE, SCREENSHOT_TOOLS,
};
pub use shortcuts::{
    shortcut_id, shortcut_slot, GlobalShortcuts, ShortcutEvent, ShortcutSpec,
    KDE_GLOBALACCEL_SERVICE, PORTAL_GLOBAL_SHORTCUTS_INTERFACE, PORTAL_REGISTRY_INTERFACE,
    PORTAL_SESSION_INTERFACE,
};
pub use tray::{
    active_app_id, install_desktop_shell, install_desktop_shell_for, publish_unread,
    record_tray_event, set_tray_muted, set_unread_badge, take_tray_events, tray_active,
    tray_backend, wait_for_tray_ready, TrayBackend, TrayEvent,
};
pub use window_controls::{
    classify_caption_style, detect_window_controls, find_window_icon_svg, parse_gtk_layout,
    parse_kde_layout, parse_xfwm_layout, CaptionStyle, WindowButton, WindowControlsLayout,
};
