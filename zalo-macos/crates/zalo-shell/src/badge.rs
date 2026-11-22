//! Menu-bar icon bytes. The Dock badge itself is an `NSDockTile` label.

use crate::error::ShellError;

/// Nominal icon edge before a host scales it.
pub const BASE_ICON_PX: u32 = 64;

/// Embedded Zalo mark. `None` only when the asset is stripped from the build.
pub const ZALO_ICON_PNG: Option<&[u8]> = Some(include_bytes!(
    "../../../../zalo-linux/crates/zalo-shell/assets/zalo-256.png"
));

/// Clamp a raw unread count the way the Linux tray does.
#[must_use]
pub fn normalize_count(raw: i64) -> u32 {
    raw.clamp(0, 99_999) as u32
}

/// Parse a count string. Non-numeric text is zero.
#[must_use]
pub fn normalize_count_text(raw: &str) -> u32 {
    raw.trim().parse::<i64>().map(normalize_count).unwrap_or(0)
}

/// Short label drawn by the Dock (`99+` above 99).
#[must_use]
pub fn badge_label(count: u32) -> String {
    if count == 0 {
        String::new()
    } else if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

/// PNG of the Zalo mark scaled to `size`.
///
/// # Errors
///
/// Returns [`ShellError::Image`] when the embedded PNG cannot be decoded.
pub fn zalo_icon_png(size: u32) -> Result<Vec<u8>, ShellError> {
    let bytes = ZALO_ICON_PNG.ok_or_else(|| ShellError::Image("missing icon".into()))?;
    let img = image::load_from_memory(bytes).map_err(|err| ShellError::Image(err.to_string()))?;
    let edge = size.max(1);
    let resized = img.resize(edge, edge, image::imageops::FilterType::Lanczos3);
    let mut out = Vec::new();
    resized
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Png,
        )
        .map_err(|err| ShellError::Image(err.to_string()))?;
    Ok(out)
}

/// Same bytes as [`zalo_icon_png`]. The Dock draws the numeric badge.
///
/// # Errors
///
/// Returns [`ShellError::Image`] when the icon cannot be encoded.
pub fn render_tray_badge(base_png: &[u8], _count: u32) -> Result<Vec<u8>, ShellError> {
    if base_png.is_empty() {
        return Err(ShellError::Image("empty icon".into()));
    }
    Ok(base_png.to_vec())
}

/// Decode a PNG to width, height, and tightly packed RGBA.
///
/// # Errors
///
/// Returns [`ShellError::Image`] when `png` is not a PNG.
pub fn png_to_rgba(png: &[u8]) -> Result<(i32, i32, Vec<u8>), ShellError> {
    let img = image::load_from_memory(png).map_err(|err| ShellError::Image(err.to_string()))?;
    let rgba = img.to_rgba8();
    Ok((rgba.width() as i32, rgba.height() as i32, rgba.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_label_caps_at_99() {
        assert_eq!(badge_label(0), "");
        assert_eq!(badge_label(3), "3");
        assert_eq!(badge_label(100), "99+");
        assert_eq!(normalize_count(-4), 0);
        assert_eq!(normalize_count_text("12"), 12);
    }

    #[test]
    fn embedded_icon_decodes() {
        let png = zalo_icon_png(32).expect("png");
        let (w, h, rgba) = png_to_rgba(&png).expect("rgba");
        assert_eq!((w, h), (32, 32));
        assert_eq!(rgba.len(), 32 * 32 * 4);
    }
}
