//! Local QR raster for the login frame. Live `request_qr` stays off until
//! this binary replaces `zalo-gui` (avoids a second IMEI/QR against the same session).

use image::{Rgba, RgbaImage};
use qrcode::types::Color;
use qrcode::QrCode;

/// Seconds from Zalo `qr_timeout` (login getLoginInfo). Overlay only after this.
pub const QR_TIMEOUT_SECS: u64 = 180;

/// Payload drawn in preview mode. Not a live login ticket.
pub const PREVIEW_QR_PAYLOAD: &str = "http://zaloapp.com/qr/l?tk=preview-gpui";

/// Quiet zone around the modules, in module units.
const QUIET: u32 = 1;

/// Why a QR bitmap could not be built.
#[derive(Debug)]
pub enum QrRasterError {
    /// Encoder rejected the payload.
    Encode(qrcode::types::QrError),
}

impl std::fmt::Display for QrRasterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(err) => write!(f, "QR encode: {err}"),
        }
    }
}

impl std::error::Error for QrRasterError {}

/// Rasterize `payload` to a Zalo-style RGBA QR bitmap (rounded finder rings +
/// `#0068FF` inner finder dots, crisp black data modules on white).
///
/// `target` is the CSS `.qr-image` edge (`LOGIN_QR_IMAGE` = 240).
pub fn rasterize_qr(payload: &str, target: u32) -> Result<RgbaImage, QrRasterError> {
    let code = QrCode::new(payload.as_bytes()).map_err(QrRasterError::Encode)?;
    let modules = code.width() as u32;
    let grid = modules + QUIET * 2;
    let scale = (target / grid).max(1);
    let edge = grid * scale;
    let mut image = RgbaImage::from_pixel(edge, edge, Rgba([255, 255, 255, 255]));
    let finders = [
        (0u32, 0u32),
        (modules.saturating_sub(7), 0u32),
        (0u32, modules.saturating_sub(7)),
    ];
    let in_finder = |x: u32, y: u32| {
        finders
            .iter()
            .any(|&(fx, fy)| x >= fx && x < fx + 7 && y >= fy && y < fy + 7)
    };
    for y in 0..modules {
        for x in 0..modules {
            if in_finder(x, y) || code[(x as usize, y as usize)] != Color::Dark {
                continue;
            }
            let left = (x + QUIET) * scale;
            let top = (y + QUIET) * scale;
            for dy in 0..scale {
                for dx in 0..scale {
                    image.put_pixel(left + dx, top + dy, Rgba([0, 0, 0, 255]));
                }
            }
        }
    }
    // Draw the 3 Zalo finder patterns: rounded black 7×7 outer ring + rounded
    // Zalo-blue (#0068FF) 3×3 inner eye.
    let black = Rgba([0, 0, 0, 255]);
    let white = Rgba([255, 255, 255, 255]);
    let zalo_blue = Rgba([0, 104, 255, 255]);
    for &(fx, fy) in &finders {
        let ox = (fx + QUIET) * scale;
        let oy = (fy + QUIET) * scale;
        fill_rounded_rect(&mut image, ox, oy, 7 * scale, 7 * scale, scale * 3 / 2, black);
        fill_rounded_rect(
            &mut image,
            ox + scale,
            oy + scale,
            5 * scale,
            5 * scale,
            scale,
            white,
        );
        fill_rounded_rect(
            &mut image,
            ox + 2 * scale,
            oy + 2 * scale,
            3 * scale,
            3 * scale,
            scale / 2,
            zalo_blue,
        );
    }
    Ok(image)
}

fn fill_rounded_rect(
    image: &mut RgbaImage,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    r: u32,
    color: Rgba<u8>,
) {
    let r = r.min(w / 2).min(h / 2) as i64;
    let r2 = r * r;
    for dy in 0..h {
        for dx in 0..w {
            let cx = if (dx as i64) < r {
                r - 1 - dx as i64
            } else if (dx as i64) >= (w as i64 - r) {
                dx as i64 - (w as i64 - r)
            } else {
                0
            };
            let cy = if (dy as i64) < r {
                r - 1 - dy as i64
            } else if (dy as i64) >= (h as i64 - r) {
                dy as i64 - (h as i64 - r)
            } else {
                0
            };
            if cx * cx + cy * cy <= r2 {
                image.put_pixel(x0 + dx, y0 + dy, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zalo_design::LOGIN_QR_IMAGE;

    #[test]
    fn preview_qr_has_dark_and_light_pixels() {
        let image = rasterize_qr(PREVIEW_QR_PAYLOAD, LOGIN_QR_IMAGE as u32).expect("qr");
        assert!(image.width() >= 120);
        assert_eq!(image.width(), image.height());
        let dark = image.pixels().filter(|px| px.0[0] < 16).count();
        let light = image.pixels().filter(|px| px.0[0] > 240).count();
        assert!(dark > 100, "expected QR modules, dark={dark}");
        assert!(light > 100, "expected quiet zone, light={light}");
    }

    #[test]
    fn timeout_matches_zalo_qr_timeout() {
        assert_eq!(QR_TIMEOUT_SECS, 180);
    }
}
