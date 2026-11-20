//! Errors from the JXL helpers.

/// A decode, resize, or encode step failed.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct JxlError(pub String);

impl JxlError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
