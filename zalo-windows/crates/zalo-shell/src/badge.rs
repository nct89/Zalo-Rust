//! Tray icon and numeric unread badge rendered in pure Rust.

use std::io::Cursor;

use image::{Rgba, RgbaImage};

use crate::error::ShellError;

const GLYPH_W: i32 = 5;
const GLYPH_H: i32 = 7;
const BADGE_RED: [u8; 3] = [240, 54, 54];
const BADGE_TEXT: [u8; 3] = [255, 255, 255];

/// Nominal icon edge before a host scales it.
pub const BASE_ICON_PX: u32 = 64;

/// Embedded Zalo mark (`256x256` PNG).
pub const ZALO_ICON_PNG: Option<&[u8]> = Some(include_bytes!(
    "../../../../zalo-linux/crates/zalo-shell/assets/zalo-256.png"
));

/// Clamp a raw unread count. Values below 1 become 0; capped at 9999.
#[must_use]
pub fn normalize_count(raw: i64) -> u32 {
    raw.clamp(0, 9999) as u32
}

/// Parse a count string. Non-numeric text is zero.
#[must_use]
pub fn normalize_count_text(raw: &str) -> u32 {
    raw.trim().parse::<i64>().map(normalize_count).unwrap_or(0)
}

/// Short label drawn on the badge (`99+` above 99, empty for 0).
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
    let rgba = zalo_icon_rgba(size)?;
    encode_png(&rgba)
}

/// RGBA image of the Zalo mark scaled to `size`.
pub(crate) fn zalo_icon_rgba(size: u32) -> Result<RgbaImage, ShellError> {
    if size == 0 {
        return Err(ShellError::Image("icon size is zero".into()));
    }
    let bytes = ZALO_ICON_PNG.ok_or_else(|| ShellError::Image("missing icon".into()))?;
    let logo = decode_png(bytes)?;
    if logo.width() == size && logo.height() == size {
        return Ok(logo);
    }
    Ok(image::imageops::resize(
        &logo,
        size,
        size,
        image::imageops::FilterType::Lanczos3,
    ))
}

/// Draw `count` on `base_png` and return the resulting RGBA buffer.
pub(crate) fn render_tray_rgba(base_png: &[u8], count: u32) -> Result<RgbaImage, ShellError> {
    let mut img = decode_png(base_png)?;
    if count == 0 || img.width() == 0 || img.height() == 0 {
        return Ok(img);
    }
    let text = badge_label(count);
    let size = img.width().min(img.height()) as f64;
    let radius = size
        * if text.len() >= 3 {
            0.30
        } else if text.len() == 2 {
            0.27
        } else {
            0.24
        };
    let cx = img.width() as f64 - radius - size * 0.04;
    let cy = radius + size * 0.04;
    fill_disc(&mut img, cx, cy, radius, BADGE_RED);
    let avail_w = radius * 1.5;
    let avail_h = radius * 1.4;
    let scale_w = (avail_w / (6.0 * text.len() as f64 - 1.0)).floor() as i32;
    let scale_h = (avail_h / f64::from(GLYPH_H)).floor() as i32;
    let scale = scale_w.min(scale_h).max(1);
    draw_text(&mut img, &text, cx, cy, scale, BADGE_TEXT);
    Ok(img)
}

/// Draw `count` on `base_png` and encode as PNG.
///
/// # Errors
///
/// Returns [`ShellError::Image`] when the icon cannot be decoded or encoded.
pub fn render_tray_badge(base_png: &[u8], count: u32) -> Result<Vec<u8>, ShellError> {
    encode_png(&render_tray_rgba(base_png, count)?)
}

/// Decode a PNG to width, height, and tightly packed RGBA.
///
/// # Errors
///
/// Returns [`ShellError::Image`] when `png` is not a valid PNG.
pub fn png_to_rgba(png: &[u8]) -> Result<(i32, i32, Vec<u8>), ShellError> {
    let rgba = decode_png(png)?;
    Ok((rgba.width() as i32, rgba.height() as i32, rgba.into_raw()))
}

fn decode_png(bytes: &[u8]) -> Result<RgbaImage, ShellError> {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map(|img| img.to_rgba8())
        .map_err(|err| ShellError::Image(err.to_string()))
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, ShellError> {
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|err| ShellError::Image(err.to_string()))?;
    Ok(out)
}

fn blend(img: &mut RgbaImage, x: u32, y: u32, rgb: [u8; 3], alpha: u8) {
    if alpha == 0 {
        return;
    }
    let pixel = img.get_pixel_mut(x, y);
    if alpha == 255 {
        *pixel = Rgba([rgb[0], rgb[1], rgb[2], 255]);
        return;
    }
    let cov = f64::from(alpha) / 255.0;
    let inv = 1.0 - cov;
    let mix = |s: u8, d: u8| (f64::from(s) * cov + f64::from(d) * inv).round() as u8;
    *pixel = Rgba([
        mix(rgb[0], pixel[0]),
        mix(rgb[1], pixel[1]),
        mix(rgb[2], pixel[2]),
        pixel[3].max(alpha),
    ]);
}

fn fill_disc(img: &mut RgbaImage, cx: f64, cy: f64, radius: f64, rgb: [u8; 3]) {
    let width = img.width() as i32;
    let height = img.height() as i32;
    if width == 0 || height == 0 {
        return;
    }
    let x0 = ((cx - radius - 1.0).floor() as i32).max(0);
    let x1 = ((cx + radius + 1.0).ceil() as i32).min(width - 1);
    let y0 = ((cy - radius - 1.0).floor() as i32).max(0);
    let y1 = ((cy + radius + 1.0).ceil() as i32).min(height - 1);
    if x1 < x0 || y1 < y0 {
        return;
    }
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = f64::from(x) + 0.5 - cx;
            let dy = f64::from(y) + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha = if dist <= radius - 1.0 {
                255
            } else if dist >= radius {
                0
            } else {
                ((radius - dist) * 255.0).round() as u8
            };
            if alpha > 0 {
                blend(img, x as u32, y as u32, rgb, alpha);
            }
        }
    }
}

fn draw_text(img: &mut RgbaImage, text: &str, cx: f64, cy: f64, scale: i32, rgb: [u8; 3]) {
    let spacing = scale;
    let total_w = text.len() as i32 * GLYPH_W * scale + (text.len() as i32 - 1) * spacing;
    let total_h = GLYPH_H * scale;
    let mut origin_x = (cx - f64::from(total_w) / 2.0).round() as i32;
    let origin_y = (cy - f64::from(total_h) / 2.0).round() as i32;
    for ch in text.chars() {
        if let Some(g) = glyph(ch) {
            draw_glyph(img, &g, origin_x, origin_y, scale, rgb);
        }
        origin_x += GLYPH_W * scale + spacing;
    }
}

fn draw_glyph(
    img: &mut RgbaImage,
    glyph: &[&str; 7],
    origin_x: i32,
    origin_y: i32,
    scale: i32,
    rgb: [u8; 3],
) {
    let width = img.width() as i32;
    let height = img.height() as i32;
    for (gy, row) in glyph.iter().enumerate() {
        for (gx, bit) in row.bytes().enumerate() {
            if bit != b'1' {
                continue;
            }
            for sy in 0..scale {
                for sx in 0..scale {
                    let px = origin_x + gx as i32 * scale + sx;
                    let py = origin_y + gy as i32 * scale + sy;
                    if px >= 0 && py >= 0 && px < width && py < height {
                        blend(img, px as u32, py as u32, rgb, 255);
                    }
                }
            }
        }
    }
}

fn glyph(ch: char) -> Option<[&'static str; 7]> {
    Some(match ch {
        '0' => [
            "01110", "10001", "10011", "10101", "11001", "10001", "01110",
        ],
        '1' => [
            "00100", "01100", "00100", "00100", "00100", "00100", "01110",
        ],
        '2' => [
            "01110", "10001", "00001", "00010", "00100", "01000", "11111",
        ],
        '3' => [
            "11110", "00001", "00001", "01110", "00001", "00001", "11110",
        ],
        '4' => [
            "00010", "00110", "01010", "10010", "11111", "00010", "00010",
        ],
        '5' => [
            "11111", "10000", "11110", "00001", "00001", "10001", "01110",
        ],
        '6' => [
            "00110", "01000", "10000", "11110", "10001", "10001", "01110",
        ],
        '7' => [
            "11111", "00001", "00010", "00100", "01000", "01000", "01000",
        ],
        '8' => [
            "01110", "10001", "10001", "01110", "10001", "10001", "01110",
        ],
        '9' => [
            "01110", "10001", "10001", "01111", "00001", "00010", "01100",
        ],
        '+' => [
            "00000", "00100", "00100", "11111", "00100", "00100", "00000",
        ],
        _ => return None,
    })
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
    fn embedded_icon_decodes_and_paints_badge() {
        let png = zalo_icon_png(32).expect("png");
        let badged = render_tray_badge(&png, 7).expect("badged");
        let (w, h, rgba) = png_to_rgba(&badged).expect("rgba");
        assert_eq!((w, h), (32, 32));
        assert_eq!(rgba.len(), 32 * 32 * 4);
    }
}
