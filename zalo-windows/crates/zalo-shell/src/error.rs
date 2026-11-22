//! Errors from the Windows shell integration.

/// The system tray, notification, dialog, or startup registry operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    /// A Win32 / COM desktop call failed.
    #[error("desktop bus is unavailable: {0}")]
    Bus(String),
    /// A badge PNG could not be decoded or encoded.
    #[error("badge image: {0}")]
    Image(String),
    /// A startup registry key or icon file could not be written or removed.
    #[error("desktop file: {0}")]
    Io(String),
}
