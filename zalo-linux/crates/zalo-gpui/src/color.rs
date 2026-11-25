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
        gpui::hsla(0., 0., 0., 0.65)
    } else {
        gpui::hsla(0., 0., 0., 0.35)
    }
}

/// Primary interactive/CTA fill: Zalo blue `#0068ff` in light mode,
/// high-contrast neutral `#f0f0f0` in dark mode (zero blue on dark).
#[must_use]
pub fn accent_bg() -> u32 {
    if is_dark() {
        0xf0_f0_f0
    } else {
        0x00_68_ff
    }
}

/// High-contrast foreground (text / icon) rendered on top of [`accent_bg`].
#[must_use]
pub fn on_accent_fg() -> u32 {
    if is_dark() {
        0x12_12_12
    } else {
        0xff_ff_ff
    }
}

/// Accent foreground for active neutral labels and chips on dark surfaces.
#[must_use]
pub fn accent_fg() -> u32 {
    if is_dark() {
        0xf0_f0_f0
    } else {
        0x00_5a_e0
    }
}

/// High-contrast blue foreground for `@mention`, URLs, phone numbers, and
/// text links (Zalo dark token `#66a6ff` — readable on dark neutral bubbles,
/// never used as a background fill on dark surfaces).
#[must_use]
pub fn link_fg() -> u32 {
    if is_dark() {
        0x66_a6_ff
    } else {
        0x00_68_ff
    }
}

/// Subtle tinted surface for active chips, selected items, and callout banners.
#[must_use]
pub fn accent_subtle_bg() -> u32 {
    if is_dark() {
        0x26_26_26
    } else {
        0xe5_f1_ff
    }
}

/// Translucent highlight for text selection and drag selection.
#[must_use]
pub fn selection_highlight() -> gpui::Hsla {
    if is_dark() {
        gpui::hsla(0., 0., 1., 0.22)
    } else {
        gpui::rgba(0x00_68_ff_44).into()
    }
}

/// Disabled primary CTA fill (translucent blue in light, subtle neutral in dark).
#[must_use]
pub fn accent_disabled_bg() -> gpui::Hsla {
    if is_dark() {
        gpui::hsla(0., 0., 1., 0.14)
    } else {
        gpui::hsla(0.6, 0.9, 0.5, 0.35)
    }
}

/// Disabled primary CTA foreground.
#[must_use]
pub fn accent_disabled_fg() -> u32 {
    if is_dark() {
        0x73_73_73
    } else {
        0xff_ff_ff
    }
}

/// Toggle switch track when OFF.
#[must_use]
pub fn switch_off_bg() -> u32 {
    if is_dark() {
        0x3a_3a_3a
    } else {
        0xc5_ca_d3
    }
}

/// Toggle switch knob color for `on` / `off` states.
#[must_use]
pub fn switch_knob_fg(on: bool) -> u32 {
    if is_dark() {
        if on {
            0x12_12_12
        } else {
            0xb8_b8_b8
        }
    } else {
        0xff_ff_ff
    }
}

/// Default fallback avatar background (neutral gray in dark mode, Zalo blue in light mode).
#[must_use]
pub fn neutral_avatar_bg() -> u32 {
    if is_dark() {
        0x33_33_33
    } else {
        0x00_68_ff
    }
}

/// Card / elevated panel surface in settings & dialogs.
#[must_use]
pub fn card_bg() -> u32 {
    if is_dark() {
        0x1f_1f_1f
    } else {
        0xf4_f6_f8
    }
}

/// Card / elevated panel hover fill.
#[must_use]
pub fn card_hover_bg() -> u32 {
    if is_dark() {
        0x29_29_29
    } else {
        0xea_ee_f4
    }
}

/// Card / elevated panel border color.
#[must_use]
pub fn card_border() -> u32 {
    if is_dark() {
        0x30_30_30
    } else {
        0xe1_e5_ea
    }
}

/// Pure-neutral (`R == G == B`) dark mode overrides: zero blue/slate/navy tints
/// on dark surfaces, bubbles, selected rows, badges, buttons, or links.
fn dark_neutral_override(name: &str) -> Option<u32> {
    Some(match name {
        // Primary chrome & canvas surfaces (pure neutral black/gray)
        "--title-bar" => 0x0a_0a_0a,
        "--layer-background-navbar-normal" | "--surface-background" => 0x0d_0d_0d,
        "--layer-background-CSC" => 0x12_12_12,
        "--surface-background-subtle"
        | "--surface-background-overlay"
        | "--layer-background-leftmenu" => 0x14_14_14,
        "--layer-background" | "--surface-alt" => 0x18_18_18,
        "--layer-background-disabled" => 0x1c_1c_1c,
        "--layer-background-subtle"
        | "--layer-background-leftmenu-hover"
        | "--layer-neutral-information-background" => 0x20_20_20,
        "--input-field-bg-filled" | "--input-field-bg-outline" => 0x22_22_22,
        "--layer-background-hover" | "--reaction-background" => 0x24_24_24,
        "--layer-background-selected"
        | "--layer-background-leftmenu-selected"
        | "--layer-information-background"
        | "--accent-blue-bg-subtle"
        | "--B10" => 0x28_28_28,
        "--input-field-bg-filled-hover" => 0x29_29_29,
        "--button-secondary-neutral-normal" => 0x2a_2a_2a,
        "--N20" => 0x2c_2c_2c,
        "--layer-background-inverse-subtle" | "--icon-hover-bg" => 0x2e_2e_2e,
        "--button-secondary-neutral-hover" => 0x36_36_36,
        "--N30" => 0x38_38_38,

        // Chat bubbles (pure neutral, clearly separated from canvas & each other)
        "--white-message" => 0x21_21_21,
        "--white-message-selected" => 0x2b_2b_2b,
        "--white-quote" => 0x1a_1a_1a,
        "--blue-message" => 0x2e_2e_2e,
        "--blue-message-selected" => 0x38_38_38,
        "--blue-quote" => 0x24_24_24,

        // Neutral text & icon hierarchy
        "--text-primary" | "--icon-primary" | "--icon-navbar-normal" => 0xe8_e8_e8,
        "--text-secondary" | "--icon-secondary" => 0xa3_a3_a3,
        "--text-placeholder" | "--text-disabled" | "--icon-disabled" => 0x73_73_73,
        "--text-quote" => 0xcc_cc_cc,
        "--text-on-color" => 0xff_ff_ff,

        // Primary interactive buttons & neutral active fills (zero blue backgrounds on dark)
        "--button-primary-normal"
        | "--accent-blue-bg"
        | "--B55"
        | "--B60"
        | "--blue" => 0xf0_f0_f0,
        "--button-primary-hover" => 0xff_ff_ff,
        "--button-primary-pressed" => 0xd4_d4_d4,
        "--button-primary-text" => 0x12_12_12,

        // Foreground-only link, @mention, phone number, and text-action blue
        // (matches Zalo PC dark token `#66a6ff` for crisp readability on dark neutral bubbles)
        "--accent-blue-text"
        | "--text-information"
        | "--link"
        | "--text-mention" => 0x66_a6_ff,

        // Semantic status colors tuned for crisp contrast on dark surfaces
        "--R60" | "--danger" | "--text-errors" => 0xef_53_50,
        "--G60" | "--G70" | "--text-success" | "--success" => 0x4a_de_80,
        "--Y60" | "--text-warning" => 0xfb_bf_24,

        // Neutral dividers / borders when resolved as opaque RGB
        "--divider" => 0x2b_2b_2b,
        "--divider-subtle" => 0x22_22_22,
        "--border" => 0x36_36_36,
        _ => return None,
    })
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
///
/// Results are cached per `(dark, name)` so repeated per-frame lookups never
/// re-parse `rgba(...)` / `hsla(...)` strings.
pub fn token_rgb(name: &str, fallback: &str) -> u32 {
    use std::collections::HashMap;
    use std::sync::{OnceLock, RwLock};

    static LIGHT_CACHE: OnceLock<RwLock<HashMap<String, Option<u32>>>> = OnceLock::new();
    static DARK_CACHE: OnceLock<RwLock<HashMap<String, Option<u32>>>> = OnceLock::new();

    let dark = is_dark();
    let cache = if dark {
        DARK_CACHE.get_or_init(|| RwLock::new(HashMap::with_capacity(96)))
    } else {
        LIGHT_CACHE.get_or_init(|| RwLock::new(HashMap::with_capacity(96)))
    };

    if let Ok(guard) = cache.read() {
        if let Some(&cached) = guard.get(name) {
            return cached.or_else(|| parse_hex(fallback)).unwrap_or(0);
        }
    }

    let resolved = if dark {
        dark_neutral_override(name).or_else(|| {
            let table = zalo_design::dark(name).or_else(|| zalo_design::light(name));
            table.and_then(|v| parse_hex(v).or_else(|| parse_alpha(v, 0x18_18_18)))
        })
    } else {
        zalo_design::light(name)
            .and_then(|v| parse_hex(v).or_else(|| parse_alpha(v, 0xff_ff_ff)))
    };

    if let Ok(mut guard) = cache.write() {
        guard.insert(name.to_owned(), resolved);
    }

    resolved.or_else(|| parse_hex(fallback)).unwrap_or(0)
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

    #[test]
    fn dark_overrides_are_pure_neutral_without_blue_tint() {
        let neutral_keys = [
            "--title-bar",
            "--layer-background-navbar-normal",
            "--layer-background-CSC",
            "--layer-background",
            "--layer-background-subtle",
            "--layer-background-hover",
            "--layer-background-selected",
            "--input-field-bg-filled",
            "--white-message",
            "--blue-message",
            "--N20",
            "--N30",
            "--B10",
            "--B55",
            "--B60",
            "--button-primary-normal",
            "--button-primary-text",
            "--accent-blue-bg",
            "--accent-blue-bg-subtle",
            "--text-primary",
            "--text-secondary",
            "--text-placeholder",
        ];
        for key in neutral_keys {
            let rgb = dark_neutral_override(key).expect(key);
            let r = (rgb >> 16) & 0xff;
            let g = (rgb >> 8) & 0xff;
            let b = rgb & 0xff;
            assert_eq!(r, g, "{key} must be pure neutral (R == G == B), got #{rgb:06x}");
            assert_eq!(g, b, "{key} must be pure neutral (R == G == B), got #{rgb:06x}");
        }
        // Foreground-only link / @mention / phone / info text uses bright blue `#66a6ff`
        // for clear visibility on dark neutral surfaces and chat bubbles.
        for key in ["--link", "--text-mention", "--text-information", "--accent-blue-text"] {
            assert_eq!(dark_neutral_override(key), Some(0x66_a6_ff), "{key}");
        }
        // High-contrast button text vs primary fill
        assert_ne!(
            dark_neutral_override("--button-primary-normal"),
            dark_neutral_override("--button-primary-text")
        );
        // Outgoing & incoming bubbles remain visually distinct
        assert_ne!(
            dark_neutral_override("--blue-message"),
            dark_neutral_override("--white-message")
        );
    }
}
