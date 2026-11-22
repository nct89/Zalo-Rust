//! No-op stand-in for the Linux X11 frame shaper.
//!
//! macOS windows are composited by the window server. GPUI draws the rounded
//! client frame; there is no `_NET_WM` shape mask to maintain.

/// macOS is always composited.
#[must_use]
pub fn is_composited() -> bool {
    true
}

/// Gives the minimized window a moment to leave the screen before a capture.
pub fn wait_app_windows_hidden(timeout_ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(timeout_ms.min(400)));
}

/// Nothing to reset. Present so call sites match Linux.
pub fn reset_settle() {}

/// Native shadows replace the X11 input shape.
pub fn anticipate_csd_window() {}

/// Nothing to sync.
pub fn sync(_scale: f32) {}

/// The window server owns the frame, so the client buffer did not change shape.
#[must_use]
pub fn sync_window(_width: i32, _height: i32, _maximized: bool, _scale: f32) -> bool {
    false
}

/// Notification windows are positioned by GPUI.
pub fn sync_notice(_scale: f32) {}

/// Pointer position in screen pixels. `None` — the popup uses GPUI's cursor.
#[must_use]
pub fn query_pointer_root() -> Option<(i32, i32)> {
    None
}

/// Top-left of the notification window in screen pixels.
#[must_use]
pub fn notice_root_origin(_scale: f32) -> Option<(f32, f32)> {
    None
}

/// GPUI moves the notification. Returning `false` keeps the in-window drag path.
#[must_use]
pub fn move_notice_to(_x: f32, _y: f32, _scale: f32) -> bool {
    false
}

/// Nothing to park off-screen.
pub fn park_notice(_scale: f32) {}

/// Nothing to pre-shape.
pub fn anticipate_notice(_scale: f32) {}
