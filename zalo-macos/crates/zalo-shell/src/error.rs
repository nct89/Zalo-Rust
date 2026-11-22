//! Errors from the macOS shell integration.

/// The menu bar, a notification, or a desktop file could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    /// A system service (`osascript`, `screencapture`, AppKit) refused the call.
    #[error("desktop bus is unavailable: {0}")]
    Bus(String),
    /// A badge PNG could not be decoded or encoded.
    #[error("badge image: {0}")]
    Image(String),
    /// A login item or icon file could not be written or removed.
    #[error("desktop file: {0}")]
    Io(String),
}
