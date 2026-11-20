//! Hex tokens from `zalo-design` → packed RGB and Vietnamese strings.

/// Parse `#rrggbb` / `rrggbb` into `0xRRGGBB`. Alpha and named CSS values are skipped.
pub fn parse_hex(hex: &str) -> Option<u32> {
    let hex = hex.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() == 3 {
        let mut packed = 0u32;
        for byte in hex.bytes() {
            let nibble = hex_nibble(byte)?;
            packed = (packed << 8) | u32::from(nibble * 17);
        }
        return Some(packed);
    }
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Catalogue text, or `fallback` when the key is missing.
/// English catalogue instead while «English» is on.
pub fn str_vi(key: &str, fallback: &'static str) -> &'static str {
    if crate::i18n::is_english() {
        if let Some(en) = zalo_design::tr_en(key).filter(|e| !e.is_empty()) {
            return en;
        }
    }
    zalo_design::tr(key).unwrap_or(fallback)
}

/// `0xRRGGBB` → opaque [`gpui::Hsla`].
pub fn rgb_hsla(c: u32) -> gpui::Hsla {
    gpui::rgb(c).into()
}

static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Switch every later [`token_rgb`] lookup to the dark table.
pub fn set_dark(on: bool) {
    DARK.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// True while the dark token table is active.
pub fn is_dark() -> bool {
    DARK.load(std::sync::atomic::Ordering::Relaxed)
}

/// Theme-aware subtle 1 px border / divider (light `rgba(0,0,0,0.12)` vs dark `hsla(0,0%,100%,0.14)`).
#[must_use]
pub fn border_subtle() -> gpui::Hsla {
    if is_dark() {
        gpui::hsla(0., 0., 1., 0.14)
    } else {
        gpui::hsla(0., 0., 0., 0.12)
    }
}

/// Hairline separator color inside panels and list dividers.
#[must_use]
pub fn border_vanish() -> gpui::Hsla {
    if is_dark() {
        gpui::hsla(0., 0., 1., 0.08)
    } else {
        gpui::hsla(0., 0., 0., 0.08)
    }
}

/// Modal backdrop scrim over the main shell.
#[must_use]
pub fn modal_scrim() -> gpui::Hsla {
    if is_dark() {
        gpui::hsla(0., 0., 0., 0.58)
    } else {
        gpui::hsla(0., 0., 0., 0.35)
    }
}

/// `rgba(r,g,b,a)` / `hsla(h,s%,l%,a)` composited over `base` (0xRRGGBB).
/// Dark-theme hover / border tokens are translucent white like these.
pub fn parse_alpha(css: &str, base: u32) -> Option<u32> {
    let css = css.trim();
    let (kind, rest) = css.split_once('(')?;
    let parts: Vec<f32> = rest
        .trim_end_matches(')')
        .split(',')
        .map(|p| p.trim().trim_end_matches('%').parse::<f32>())
        .collect::<Result<_, _>>()
        .ok()?;
    let (r, g, b, a) = match (kind.trim(), parts.as_slice()) {
        ("rgba" | "rgb", [r, g, b, rest @ ..]) => {
            (*r, *g, *b, rest.first().copied().unwrap_or(1.0))
        }
        ("hsla" | "hsl", [h, s, l, rest @ ..]) => {
            let (h, s, l) = (h / 360.0, s / 100.0, l / 100.0);
            let q = if l < 0.5 {
                l * (1.0 + s)
            } else {
                l + s - l * s
            };
            let p = 2.0 * l - q;
            let ch = |mut t: f32| {
                if t < 0.0 {
                    t += 1.0;
                }
                if t > 1.0 {
                    t -= 1.0;
                }
                if t < 1.0 / 6.0 {
                    p + (q - p) * 6.0 * t
                } else if t < 0.5 {
                    q
                } else if t < 2.0 / 3.0 {
                    p + (q - p) * (2.0 / 3.0 - t) * 6.0
                } else {
                    p
                }
            };
            (
                ch(h + 1.0 / 3.0) * 255.0,
                ch(h) * 255.0,
                ch(h - 1.0 / 3.0) * 255.0,
                rest.first().copied().unwrap_or(1.0),
            )
        }
        _ => return None,
    };
    let mix = |fg: f32, shift: u32| {
        let bg = ((base >> shift) & 0xff) as f32;
        ((fg * a + bg * (1.0 - a)).round().clamp(0.0, 255.0) as u32) << shift
    };
    Some(mix(r, 16) | mix(g, 8) | mix(b, 0))
}

/// Theme token as `0xRRGGBB` (dark table when [`set_dark`]), or the fallback.
/// Translucent tokens are flattened over the panel background.
pub fn token_rgb(name: &str, fallback: &str) -> u32 {
    let dark = is_dark();
    let table = if dark {
        zalo_design::dark(name).or_else(|| zalo_design::light(name))
    } else {
        zalo_design::light(name)
    };
    let base = if dark { 0x22_26_2b } else { 0xff_ff_ff };
    table
        .and_then(|v| parse_hex(v).or_else(|| parse_alpha(v, base)))
        .or_else(|| parse_hex(fallback))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translucent_tokens_flatten() {
        // Global theme flag untouched: tests run in parallel.
        assert_eq!(
            parse_alpha("hsla(0,0%,100%,0.05)", 0x22262b),
            Some(0x2d_31_36)
        );
        assert_eq!(parse_alpha("rgba(0,0,0,0.1)", 0xffffff), Some(0xe6_e6_e6));
        let dark_hover = zalo_design::dark("--layer-background-hover").unwrap();
        assert_eq!(parse_alpha(dark_hover, 0x22262b), Some(0x2d_31_36));
    }

    #[test]
    fn parse_zalo_blue() {
        assert_eq!(parse_hex("#005ae0"), Some(0x005a_e0));
        assert_eq!(parse_hex("0068ff"), Some(0x0068_ff));
        assert_eq!(parse_hex("#abc"), Some(0xaabb_cc));
        assert_eq!(parse_hex("rgba(0,0,0,0.15)"), None);
    }

    #[test]
    fn login_strings_are_in_the_catalogue() {
        assert_eq!(
            str_vi("STR_LOGIN_QR_PAGE_TITLE", ""),
            "Quét mã QR để đăng nhập"
        );
        assert_eq!(
            str_vi("STR_LOGIN_QR_PAGE_QR_TIMEOUT_ERROR", ""),
            "Mã QR đã hết hạn"
        );
        assert_eq!(
            str_vi("STR_LOGIN_QR_PAGE_RETRY_GET_QR", ""),
            "Bấm để tải lại"
        );
    }

    #[test]
    fn light_tokens_match_css() {
        assert_eq!(token_rgb("--B60", "#000"), 0x005a_e0);
        assert_eq!(token_rgb("--text-primary", "#000"), 0x081b_3a);
        assert_eq!(token_rgb("--text-errors", "#000"), 0xc318_18);
        assert_eq!(token_rgb("--input-field-bg-filled", "#000"), 0xf1f2_f4);
        assert_eq!(token_rgb("--blue-message", "#000"), 0xe5f1_ff);
        assert_eq!(token_rgb("--white-message", "#000"), 0xff_ffff);
    }
}
