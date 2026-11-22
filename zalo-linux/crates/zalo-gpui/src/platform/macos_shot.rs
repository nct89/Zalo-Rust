//! Screenshot types shared with the Linux overlay. Capture itself is `screencapture`.

use std::path::PathBuf;

use gpui::App;

/// Outcome of the in-app crop overlay. macOS uses `screencapture -i` instead.
#[derive(Clone, Debug)]
pub(crate) enum ShotOutcome {
    /// Send the PNG.
    Send { path: PathBuf, png_bytes: Vec<u8> },
    /// Copy only.
    Copied { png_bytes: Vec<u8> },
    /// Saved under Pictures.
    Saved { path: PathBuf, png_bytes: Vec<u8> },
    /// The user cancelled.
    Cancelled,
}

/// One grabbed monitor. Unused when [`native_x11_available`] is false.
pub(crate) struct CapturedMonitor {
    /// Left edge.
    pub x: i16,
    /// Top edge.
    pub y: i16,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// BGRA pixels.
    pub bgra: Vec<u8>,
    /// Window rectangles in monitor-local pixels.
    pub windows: Vec<(f32, f32, f32, f32)>,
}

/// The X11 overlay is not used on macOS.
#[must_use]
pub(crate) fn native_x11_available() -> bool {
    false
}

/// Not used on macOS.
///
/// # Errors
///
/// Always fails so the caller falls through to `screencapture`.
pub(crate) fn grab_active_monitor() -> Result<CapturedMonitor, String> {
    Err("macOS dùng screencapture".into())
}

/// Not used on macOS.
///
/// # Errors
///
/// Always fails.
pub(crate) fn open_screenshot_overlay(
    _capture: CapturedMonitor,
    _on_done: impl FnOnce(ShotOutcome, &mut App) + 'static,
    _cx: &mut App,
) -> Result<(), String> {
    Err("macOS dùng screencapture".into())
}
