//! Errors from the tray and the session bus.

/// The tray or a desktop notification could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    /// The session bus refused the call or is not running.
    #[error("desktop bus is unavailable: {0}")]
    Bus(String),
    /// A badge PNG could not be decoded or encoded.
    #[error("badge image: {0}")]
    Image(String),
    /// A desktop file could not be written or removed.
    #[error("desktop file: {0}")]
    Io(String),
}
