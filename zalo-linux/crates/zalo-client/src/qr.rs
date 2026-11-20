//! Terminal QR rendering.

use qrcode::render::unicode;
use qrcode::QrCode;
use thiserror::Error;

/// Why a QR code could not be rendered.
#[derive(Debug, Error)]
pub enum QrError {
    /// The input is too large or otherwise not encodable.
    #[error("QR encoding failed: {0}")]
    Encode(#[from] qrcode::types::QrError),
}

/// Renders `text` as a block QR code for the terminal.
///
/// # Errors
///
/// Returns [`QrError`] when the QR encoder rejects the input.
pub fn render_terminal(text: &str) -> Result<String, QrError> {
    let code = QrCode::new(text.as_bytes())?;
    Ok(code.render::<unicode::Dense1x2>().quiet_zone(true).build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_qr_block() {
        let rendered = render_terminal("http://zaloapp.com/qr/l?tk=test").expect("render");
        assert!(rendered.lines().count() > 10);
    }
}
