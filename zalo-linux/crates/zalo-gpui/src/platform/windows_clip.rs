//! Clipboard writes through GPUI's native Win32 clipboard backend.

use std::path::Path;

use gpui::{App, ClipboardItem, Image, ImageFormat};

/// Decodes a cached media file into PNG, or keeps PNG/GIF bytes.
///
/// # Errors
///
/// Returns a Vietnamese message when the file cannot be read or decoded.
pub fn encode_image_file(path: &Path) -> Result<(ImageFormat, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    match image::guess_format(&bytes) {
        Ok(image::ImageFormat::Png) => Ok((ImageFormat::Png, bytes)),
        Ok(image::ImageFormat::Gif) => Ok((ImageFormat::Gif, bytes)),
        _ => {
            let mut png = Vec::new();
            let rgba = crate::media::decode_rgba(&bytes, 8192)
                .ok_or_else(|| "Không đọc được ảnh".to_owned())?;
            rgba.write_to(
                &mut std::io::Cursor::new(&mut png),
                image::ImageFormat::Png,
            )
            .map_err(|err| err.to_string())?;
            Ok((ImageFormat::Png, png))
        }
    }
}

/// Copy plain text onto the Windows clipboard.
pub fn copy_text(cx: &mut App, text: String) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// Copy an encoded image onto the Windows clipboard.
pub fn copy_image(cx: &mut App, format: ImageFormat, bytes: Vec<u8>) {
    cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_image_file_keeps_png() {
        let dir = std::env::temp_dir().join(format!("zalo-win-clip-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]));
        let path = dir.join("sample.png");
        img.save_with_format(&path, image::ImageFormat::Png).unwrap();
        let (fmt, bytes) = encode_image_file(&path).unwrap();
        assert_eq!(fmt, ImageFormat::Png);
        assert_eq!(image::guess_format(&bytes).unwrap(), image::ImageFormat::Png);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
