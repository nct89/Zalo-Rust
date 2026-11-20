//! Unread count painted onto a tray icon.
//!
//! StatusNotifierItem has no numeric badge, so `plugins/tray-badge` draws a
//! red disc and a 5×7 bitmap font into the icon. The base icon here is a
//! generated blue square. Zalo's PNG artwork is not copied into this crate.

use std::io::Cursor;

use image::{Rgba, RgbaImage};

use crate::error::ShellError;

const GLYPH_W: i32 = 5;
const GLYPH_H: i32 = 7;
const BADGE_RED: [u8; 3] = [240, 54, 54];
const BADGE_TEXT: [u8; 3] = [255, 255, 255];

/// Side of the generated tray icon, in pixels.
pub const BASE_ICON_PX: u32 = 64;

/// Parse a count the way the tray and launcher plugins do.
///
/// Values below 1 become 0. Values above 9999 become 9999.
pub fn normalize_count(raw: i64) -> u32 {
    if raw < 1 {
        0
    } else {
        raw.min(9999) as u32
    }
}

/// `parseInt(raw, 10)` then [`normalize_count`].
///
/// Leading spaces are skipped. An optional sign is accepted. Digits stop at
/// the first non-digit, so `"12px"` is 12. A missing number is 0.
pub fn normalize_count_text(raw: &str) -> u32 {
    let trimmed = raw.trim_start();
    let (rest, negative) = if let Some(rest) = trimmed.strip_prefix('-') {
        (rest, true)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (rest, false)
    } else {
        (trimmed, false)
    };
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        return 0;
    }
    let magnitude: i64 = match digits.parse() {
        Ok(value) => value,
        Err(_) => return if negative { 0 } else { 9999 },
    };
    normalize_count(if negative { -magnitude } else { magnitude })
}

/// Label drawn in the disc. Counts above 99 read `99+`.
pub fn badge_label(count: u32) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

/// Zalo app logo, 256×256 RGBA (`pc-dist/favicon-256x256.png` of the Zalo PC
/// bundle), when `assets/zalo-256.png` was present at build time (see `build.rs`).
#[cfg(zalo_logo)]
pub const ZALO_ICON_PNG: Option<&[u8]> = Some(include_bytes!("../assets/zalo-256.png"));
/// No logo file at build time.
#[cfg(not(zalo_logo))]
pub const ZALO_ICON_PNG: Option<&[u8]> = None;

/// The Zalo logo as RGBA pixels, `size`×`size`.
///
/// # Errors
///
/// Returns [`ShellError::Image`] for a zero size or an undecodable logo.
pub fn zalo_icon_rgba(size: u32) -> Result<RgbaImage, ShellError> {
    if size == 0 {
        return Err(ShellError::Image("icon size is zero".to_owned()));
    }
    let bytes = ZALO_ICON_PNG
        .ok_or_else(|| ShellError::Image("built without assets/zalo-256.png".to_owned()))?;
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

/// The Zalo logo as a PNG, `size`×`size`: the tray base under the unread badge.
///
/// # Errors
///
/// Returns [`ShellError::Image`] for a zero size or an encoding failure.
pub fn zalo_icon_png(size: u32) -> Result<Vec<u8>, ShellError> {
    encode_png(&zalo_icon_rgba(size)?)
}

/// Solid `#0068ff` PNG used when the app has no separate logo file.
pub fn default_base_png(size: u32) -> Result<Vec<u8>, ShellError> {
    if size == 0 {
        return Err(ShellError::Image("icon size is zero".to_owned()));
    }
    let image = RgbaImage::from_pixel(size, size, Rgba([0, 104, 255, 255]));
    encode_png(&image)
}

/// Draw `count` on `base_png` and return the resulting RGBA8 buffer.
pub(crate) fn render_tray_rgba(base_png: &[u8], count: u32) -> Result<RgbaImage, ShellError> {
    let mut image = decode_png(base_png)?;
    if count == 0 || image.width() == 0 || image.height() == 0 {
        return Ok(image);
    }
    let text = badge_label(count);
    let size = image.width().min(image.height()) as f64;
    let radius = size
        * if text.len() >= 3 {
            0.30
        } else if text.len() == 2 {
            0.27
        } else {
            0.24
        };
    let cx = image.width() as f64 - radius - size * 0.04;
    let cy = radius + size * 0.04;
    fill_disc(&mut image, cx, cy, radius, BADGE_RED);
    let avail_w = radius * 1.5;
    let avail_h = radius * 1.4;
    let scale_w = (avail_w / (6.0 * text.len() as f64 - 1.0)).floor() as i32;
    let scale_h = (avail_h / f64::from(GLYPH_H)).floor() as i32;
    let scale = scale_w.min(scale_h).max(1);
    draw_text(&mut image, &text, cx, cy, scale, BADGE_TEXT);
    Ok(image)
}

/// Draw `count` on `base_png`. Zero returns the same pixels, re-encoded.
pub fn render_tray_badge(base_png: &[u8], count: u32) -> Result<Vec<u8>, ShellError> {
    encode_png(&render_tray_rgba(base_png, count)?)
}

/// Convert `image` into little-endian X11 ZPixmap bytes scaled to `width × height`.
///
/// When `argb32` is true, outputs premultiplied BGRA (for 32-bit TrueColor visuals);
/// otherwise composites semi-transparent edges over `bg_rgb` (`[r, g, b]`) into BGRX.
#[cfg(feature = "tray-xembed")]
pub(crate) fn rgba_to_x11_zpixmap(
    image: &RgbaImage,
    width: u16,
    height: u16,
    argb32: bool,
    bg_rgb: [u8; 3],
) -> Vec<u8> {
    let w = u32::from(width).max(1);
    let h = u32::from(height).max(1);
    let resized = image::imageops::resize(image, w, h, image::imageops::FilterType::Lanczos3);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for [r, g, b, a] in resized.pixels().map(|p| p.0) {
        if argb32 {
            let alpha = u16::from(a);
            let pm = |c: u8| -> u8 { ((u16::from(c) * alpha + 127) / 255) as u8 };
            out.extend_from_slice(&[pm(b), pm(g), pm(r), a]);
        } else {
            let inv = u16::from(255 - a);
            let alpha = u16::from(a);
            let comp = |fg: u8, bg: u8| -> u8 {
                ((u16::from(fg) * alpha + u16::from(bg) * inv + 127) / 255) as u8
            };
            out.extend_from_slice(&[
                comp(b, bg_rgb[2]),
                comp(g, bg_rgb[1]),
                comp(r, bg_rgb[0]),
                255,
            ]);
        }
    }
    out
}

/// ARGB32 bytes, network order, for a StatusNotifier pixmap.
pub fn png_to_argb(png: &[u8]) -> Result<(i32, i32, Vec<u8>), ShellError> {
    let image = decode_png(png)?;
    let width = image.width() as i32;
    let height = image.height() as i32;
    let mut data = image.into_raw();
    for pixel in data.chunks_exact_mut(4) {
        pixel.rotate_right(1);
    }
    Ok((width, height, data))
}

fn decode_png(bytes: &[u8]) -> Result<RgbaImage, ShellError> {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map(|image| image.to_rgba8())
        .map_err(|error| ShellError::Image(error.to_string()))
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, ShellError> {
    let mut encoded = Vec::new();
    image::DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut Cursor::new(&mut encoded), image::ImageFormat::Png)
        .map_err(|error| ShellError::Image(error.to_string()))?;
    Ok(encoded)
}

fn blend(image: &mut RgbaImage, x: u32, y: u32, rgb: [u8; 3], alpha: u8) {
    if alpha == 0 {
        return;
    }
    let pixel = image.get_pixel_mut(x, y);
    if alpha == 255 {
        *pixel = Rgba([rgb[0], rgb[1], rgb[2], 255]);
        return;
    }
    let coverage = f64::from(alpha) / 255.0;
    let inverse = 1.0 - coverage;
    let mix = |source: u8, dest: u8| {
        (f64::from(source) * coverage + f64::from(dest) * inverse).round() as u8
    };
    let red = mix(rgb[0], pixel[0]);
    let green = mix(rgb[1], pixel[1]);
    let blue = mix(rgb[2], pixel[2]);
    let dest_alpha = pixel[3].max(alpha);
    *pixel = Rgba([red, green, blue, dest_alpha]);
}

fn fill_disc(image: &mut RgbaImage, cx: f64, cy: f64, radius: f64, rgb: [u8; 3]) {
    let width = image.width() as i32;
    let height = image.height() as i32;
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
                blend(image, x as u32, y as u32, rgb, alpha);
            }
        }
    }
}

fn draw_text(image: &mut RgbaImage, text: &str, cx: f64, cy: f64, scale: i32, rgb: [u8; 3]) {
    let spacing = scale;
    let total_w = text.len() as i32 * GLYPH_W * scale + (text.len() as i32 - 1) * spacing;
    let total_h = GLYPH_H * scale;
    let mut origin_x = (cx - f64::from(total_w) / 2.0).round() as i32;
    let origin_y = (cy - f64::from(total_h) / 2.0).round() as i32;
    for ch in text.chars() {
        if let Some(glyph) = glyph(ch) {
            draw_glyph(image, &glyph, origin_x, origin_y, scale, rgb);
        }
        origin_x += GLYPH_W * scale + spacing;
    }
}

fn draw_glyph(
    image: &mut RgbaImage,
    glyph: &[&str; 7],
    origin_x: i32,
    origin_y: i32,
    scale: i32,
    rgb: [u8; 3],
) {
    let width = image.width() as i32;
    let height = image.height() as i32;
    for (gy, row) in glyph.iter().enumerate() {
        for (gx, bit) in row.bytes().enumerate() {
            if bit != b'1' {
                continue;
            }
            for sy in 0..scale {
                for sx in 0..scale {
                    let px = origin_x + gx as i32 * scale + sx;
                    let py = origin_y + gy as i32 * scale + sy;
                    if px < 0 || py < 0 || px >= width || py >= height {
                        continue;
                    }
                    blend(image, px as u32, py as u32, rgb, 255);
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
            "11111", "00010", "00100", "00010", "00001", "10001", "01110",
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
    fn counts_match_the_plugin_cap() {
        assert_eq!(normalize_count(0), 0);
        assert_eq!(normalize_count(-4), 0);
        assert_eq!(normalize_count(12), 12);
        assert_eq!(normalize_count(10_000), 9999);
        assert_eq!(normalize_count_text("12px"), 12);
        assert_eq!(normalize_count_text("  -2"), 0);
        assert_eq!(normalize_count_text("+8"), 8);
        assert_eq!(normalize_count_text("no"), 0);
        assert_eq!(badge_label(7), "7");
        assert_eq!(badge_label(100), "99+");
    }

    #[test]
    fn a_positive_count_paints_red_and_zero_does_not() {
        let base = default_base_png(BASE_ICON_PX).unwrap();
        let plain = render_tray_badge(&base, 0).unwrap();
        let plain = decode_png(&plain).unwrap();
        assert!(plain.pixels().all(|pixel| pixel.0 == [0, 104, 255, 255]));
        let marked = render_tray_badge(&base, 5).unwrap();
        let marked = decode_png(&marked).unwrap();
        assert!(marked
            .pixels()
            .any(|pixel| pixel[0] > 200 && pixel[1] < 80 && pixel[2] < 80));
        assert_eq!(marked.get_pixel(2, 60).0, [0, 104, 255, 255]);
        let many = render_tray_badge(&base, 120).unwrap();
        let many = decode_png(&many).unwrap();
        assert!(many
            .pixels()
            .any(|pixel| pixel[0] > 200 && pixel[1] < 80 && pixel[2] < 80));
    }

    #[cfg(feature = "tray-xembed")]
    #[test]
    fn converts_rgba_to_premultiplied_and_composited_x11_zpixmap() {
        let img = RgbaImage::from_pixel(2, 2, Rgba([200, 100, 50, 128]));
        let argb = rgba_to_x11_zpixmap(&img, 2, 2, true, [0, 0, 0]);
        assert_eq!(argb.len(), 16);
        assert_eq!(argb[3], 128);
        assert!(argb[2] <= 102 && argb[2] >= 99);

        let rgb24 = rgba_to_x11_zpixmap(&img, 2, 2, false, [20, 40, 60]);
        assert_eq!(rgb24.len(), 16);
        assert_eq!(rgb24[3], 255);
    }
}
