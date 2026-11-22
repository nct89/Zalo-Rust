//! macOS desktop integration for the GPUI client.
//!
//! The library name is `zalo_shell`, matching the Linux crate, so `zalo-gpui`
//! can depend on whichever package the target OS selects.

#![allow(unsafe_code)]

mod badge;
mod desktop;
mod dialogs;
mod error;
mod instance;
mod integrate;
mod notify;
#[cfg(target_os = "macos")]
mod status_item;
mod tray;

pub use badge::{
    badge_label, normalize_count, normalize_count_text, png_to_rgba, render_tray_badge,
    zalo_icon_png, BASE_ICON_PX, ZALO_ICON_PNG,
};
pub use desktop::{
    current_desktop, desktop_shell_status, detect_window_controls, find_window_icon_svg,
    format_shell_hint, is_flatpak, request_background, request_background_async, shortcut_id,
    shortcut_slot, BackgroundGrant, CaptionStyle, DesktopEnvironment, DesktopShellStatus,
    GlobalShortcuts, ShortcutEvent, ShortcutSpec, WindowButton, WindowControlsLayout,
    KDE_GLOBALACCEL_SERVICE, PORTAL_BACKGROUND_INTERFACE, PORTAL_GLOBAL_SHORTCUTS_INTERFACE,
    PORTAL_REGISTRY_INTERFACE, PORTAL_SESSION_INTERFACE,
};
pub use dialogs::{
    capture_fullscreen, capture_interactive, capture_screenshot, parse_dialog_output,
    parse_dialog_paths, pick_file, pick_folder, pick_image, pick_images, zenity_filter,
    IMAGE_PATTERNS, PORTAL_FILECHOOSER_INTERFACE, SCREENSHOT_TOOLS,
};
pub use error::ShellError;
pub use instance::{
    instance_command_from_args, instance_socket_path, ping_instance, profile_arg_from_args,
    start_instance_listener, stop_instance_listener, try_send_instance_command, InstanceCommand,
};
pub use integrate::{
    autostart_enabled, autostart_enabled_at, autostart_path, data_home, install_app_entry,
    install_app_entry_at, install_gpui_app_entry, launch_agent_plist, set_autostart,
    set_autostart_for, set_autostart_for_at, APP_ID, GPUI_APP_ID, ICON_SIZES,
};
pub use notify::{notify_call, notify_desktop, unread_notice, DesktopNotice, NotifyCall};
pub use tray::{
    active_app_id, install_desktop_shell, install_desktop_shell_for, publish_unread,
    record_tray_event, set_tray_muted, set_unread_badge, take_tray_events, tray_active,
    tray_backend, wait_for_tray_ready, TrayBackend, TrayEvent,
};
