//! Linux `#titleBar` 38 px. Login: min+close. Main: min+max+close, gray only
//! to the right of `#main-tab`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{
    div, hsla, img, prelude::*, px, rgb, App, InteractiveElement, MouseButton, ParentElement,
    RenderImage, StatefulInteractiveElement, Styled, Window,
};
use zalo_shell::{CaptionStyle, WindowButton, WindowControlsLayout};

use crate::color::token_rgb;
use crate::window_frame::CsdCorners;

pub(crate) fn zalo_title_icon() -> Option<Arc<RenderImage>> {
    static ICON: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
    ICON.get_or_init(|| {
        let bytes = zalo_shell::ZALO_ICON_PNG?;
        let rgba = crate::media::decode_rgba(bytes, 96)?;
        Some(Arc::new(RenderImage::new([image::Frame::new(
            crate::media::rgba_to_bgra(rgba),
        )])))
    })
    .clone()
}

/// Installer-window caption bar: Zalo logo, title, min and close only.
pub(crate) fn installer_titlebar(corners: CsdCorners, title: String) -> impl gpui::IntoElement {
    caption_bar(
        corners,
        false,
        false,
        None,
        None,
        true,
        true,
        title,
        zalo_title_icon(),
        false,
    )
}

/// Separate `Zalo Photo` window caption bar matching Electron's popup chrome:
/// Zalo icon + `Zalo Photo` title on the left, native min/max/close controls on the right.
pub(crate) fn photo_titlebar(corners: CsdCorners, window: &Window) -> impl gpui::IntoElement {
    caption_bar(
        corners,
        true,
        window.is_maximized(),
        None,
        None,
        true,
        true,
        "Zalo Photo".to_owned(),
        zalo_title_icon(),
        false,
    )
}

/// Gray Linux caption bar. Login window: min + close only.
/// `leading` is the hamburger on the left of the drag region.
#[allow(dead_code)]
pub fn login_titlebar(
    corners: CsdCorners,
    leading: impl gpui::IntoElement,
) -> impl gpui::IntoElement {
    caption_bar(
        corners,
        false,
        false,
        Some(leading.into_any_element()),
        None,
        true,
        true,
        String::new(),
        None,
        false,
    )
}

fn flag_icon(english: bool) -> Option<Arc<RenderImage>> {
    if english {
        static EN: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
        EN.get_or_init(|| {
            let rgba = crate::media::decode_rgba(include_bytes!("../assets/flag-en.png"), 64)?;
            Some(Arc::new(RenderImage::new([image::Frame::new(
                crate::media::rgba_to_bgra(rgba),
            )])))
        })
        .clone()
    } else {
        static VI: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
        VI.get_or_init(|| {
            let rgba = crate::media::decode_rgba(include_bytes!("../assets/flag-vi.png"), 64)?;
            Some(Arc::new(RenderImage::new([image::Frame::new(
                crate::media::rgba_to_bgra(rgba),
            )])))
        })
        .clone()
    }
}

/// Main-window caption bar with compact VI/EN flag switch, Light/Dark theme switch,
/// and clickable build version badge (e.g. `v0.2.1`).
#[allow(clippy::too_many_arguments)]
pub fn main_titlebar_with_theme(
    corners: CsdCorners,
    window: &Window,
    title: String,
    to_tray: bool,
    english: bool,
    on_toggle_lang: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_toggle_theme: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_open_about: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl gpui::IntoElement {
    let controls = div()
        .mr(px(8.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .child(lang_switch(english, on_toggle_lang))
        .child(theme_switch(on_toggle_theme))
        .child(version_badge(on_open_about))
        .into_any_element();
    caption_bar(
        corners,
        true,
        window.is_maximized(),
        None,
        Some(controls),
        false,
        true,
        title,
        None,
        to_tray,
    )
}

fn version_badge(
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl gpui::IntoElement {
    let bg = token_rgb("--layer-background", "#ffffff");
    let hover_bg = token_rgb("--layer-background-hover", "#f1f2f4");
    let fg = token_rgb("--text-secondary", "#5a6981");
    let tip_text = "Thông tin phiên bản";
    div()
        .id("caption-version-badge")
        .h(px(20.))
        .px(px(7.))
        .rounded(px(10.))
        .bg(rgb(bg))
        .border_1()
        .border_color(crate::color::border_subtle())
        .hover(move |s| s.bg(rgb(hover_bg)))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .line_height(px(11.))
        .font_family(crate::fonts::FAMILY_SEMIBOLD)
        .text_color(rgb(fg))
        .cursor_pointer()
        .tooltip(move |_, cx| crate::widgets::tip(tip_text, cx))
        .on_click(on_click)
        .child(crate::APP_VERSION_TAG)
}

fn lang_switch(
    english: bool,
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl gpui::IntoElement {
    let dark = crate::color::is_dark();
    let track = if dark { 0x38_3c_42 } else { 0xd9_dd_e3 };
    let tip_text = if english {
        "Language: English"
    } else {
        "Ngôn ngữ: Tiếng Việt"
    };
    let flag = flag_icon(english);
    div()
        .id("caption-lang-toggle")
        .w(px(36.))
        .h(px(20.))
        .px(px(2.))
        .rounded(px(10.))
        .bg(rgb(track))
        .flex()
        .flex_row()
        .items_center()
        .when(english, |el| el.justify_end())
        .when(!english, |el| el.justify_start())
        .cursor_pointer()
        .tooltip(move |_, cx| crate::widgets::tip(tip_text, cx))
        .on_click(on_toggle)
        .child(
            div()
                .size(px(16.))
                .rounded(px(8.))
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center()
                .when_some(flag, |el, f| {
                    el.child(
                        img(f)
                            .size(px(16.))
                            .rounded(px(8.))
                            .object_fit(gpui::ObjectFit::Cover),
                    )
                }),
        )
}

fn theme_switch(
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl gpui::IntoElement {
    let dark = crate::color::is_dark();
    let track = if dark { 0x38_3c_42 } else { 0xd9_dd_e3 };
    let knob = 0xff_ff_ff;
    let tip_text = "Giao diện";
    div()
        .id("caption-theme-toggle")
        .w(px(36.))
        .h(px(20.))
        .px(px(2.))
        .rounded(px(10.))
        .bg(rgb(track))
        .flex()
        .flex_row()
        .items_center()
        .when(dark, |el| el.justify_end())
        .when(!dark, |el| el.justify_start())
        .cursor_pointer()
        .tooltip(move |_, cx| crate::widgets::tip(tip_text, cx))
        .on_click(on_toggle)
        .child(
            div()
                .size(px(16.))
                .rounded(px(8.))
                .overflow_hidden()
                .relative()
                .bg(rgb(knob))
                .child(theme_mark(dark)),
        )
}

/// Font Awesome solid moon on the dark theme, solid sun on the light theme.
/// The face is weight 900 (`fa-solid-900`). A missing face falls back to a
/// text sun or moon so the knob is never empty.
fn theme_mark(dark: bool) -> gpui::AnyElement {
    let loaded = crate::fonts::awesome_loaded();
    let (glyph, color) = if dark {
        let mark = if loaded { crate::fonts::FA_MOON } else { '☾' };
        (mark, rgb(0x28_2d_33))
    } else {
        let mark = if loaded { crate::fonts::FA_SUN } else { '☀' };
        (mark, rgb(0x6b_72_80))
    };
    div()
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.))
        .line_height(px(10.))
        .text_color(color)
        .when(loaded, |el| {
            el.font_family(crate::fonts::FAMILY_AWESOME)
                .font_weight(gpui::FontWeight::BLACK)
        })
        .child(glyph.to_string())
        .into_any_element()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CaptionIconKind {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl CaptionIconKind {
    const fn symbolic_name(self) -> &'static str {
        match self {
            Self::Minimize => "window-minimize-symbolic",
            Self::Maximize => "window-maximize-symbolic",
            Self::Restore => "window-restore-symbolic",
            Self::Close => "window-close-symbolic",
        }
    }

    const fn fallback_glyph(self) -> &'static str {
        match self {
            Self::Minimize => "−",
            Self::Maximize => "□",
            Self::Restore => "❐",
            Self::Close => "×",
        }
    }
}

fn distro_controls() -> &'static WindowControlsLayout {
    static LAYOUT: OnceLock<WindowControlsLayout> = OnceLock::new();
    LAYOUT.get_or_init(|| {
        if cfg!(test) {
            WindowControlsLayout::default()
        } else {
            zalo_shell::detect_window_controls()
        }
    })
}

fn rasterize_symbolic_svg(svg_bytes: &[u8], size_px: u32, color_rgb: u32) -> Option<Arc<RenderImage>> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg_bytes, &opt).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size_px, size_px)?;
    let ts = tree.size();
    let scale = (size_px as f32 / ts.width().max(1.0)).min(size_px as f32 / ts.height().max(1.0));
    let tx = (size_px as f32 - ts.width() * scale) * 0.5;
    let ty = (size_px as f32 - ts.height() * scale) * 0.5;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // GTK symbolic icon tinting: keep smooth anti-aliased alpha coverage,
    // tint RGB to exact foreground color in straight BGRA for GPUI RenderImage.
    let r = ((color_rgb >> 16) & 0xff) as u8;
    let g = ((color_rgb >> 8) & 0xff) as u8;
    let b = (color_rgb & 0xff) as u8;
    let src = pixmap.data();
    let mut bgra = vec![0_u8; src.len()];
    for (s, d) in src.chunks_exact(4).zip(bgra.chunks_exact_mut(4)) {
        let a = s[3];
        if a > 0 {
            d[0] = b;
            d[1] = g;
            d[2] = r;
            d[3] = a;
        }
    }
    let frame = image::RgbaImage::from_raw(size_px, size_px, bgra)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(frame)])))
}

fn caption_icon_image(kind: CaptionIconKind, color_rgb: u32) -> Option<Arc<RenderImage>> {
    static CACHE: OnceLock<Mutex<HashMap<(CaptionIconKind, u32), Option<Arc<RenderImage>>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&(kind, color_rgb)) {
            return hit.clone();
        }
    }
    let layout = distro_controls();
    let rendered = zalo_shell::find_window_icon_svg(layout.icon_theme.as_deref(), kind.symbolic_name())
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| rasterize_symbolic_svg(&bytes, 32, color_rgb));
    if let Ok(mut guard) = cache.lock() {
        guard.insert((kind, color_rgb), rendered.clone());
    }
    rendered
}

#[allow(clippy::too_many_arguments)]
fn caption_bar(
    corners: CsdCorners,
    with_zoom: bool,
    maximized: bool,
    leading: Option<gpui::AnyElement>,
    trailing: Option<gpui::AnyElement>,
    round_tl: bool,
    round_tr: bool,
    title: String,
    title_icon: Option<Arc<RenderImage>>,
    to_tray: bool,
) -> impl gpui::IntoElement {
    let dark = crate::color::is_dark();
    let bar = token_rgb("--title-bar", "#e5e7eb");
    let ink = token_rgb("--text-primary", "#081b3a");
    let h = px(zalo_design::LINUX_TITLEBAR_H);
    let layout = distro_controls();

    let left_buttons: Vec<WindowButton> = layout
        .left
        .iter()
        .copied()
        .filter(|b| with_zoom || *b != WindowButton::Maximize)
        .collect();
    let right_buttons: Vec<WindowButton> = layout
        .right
        .iter()
        .copied()
        .filter(|b| with_zoom || *b != WindowButton::Maximize)
        .collect();
    let style = layout.style;

    corners
        .apply(
            div()
                .id("titleBar")
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .h(h)
                .bg(rgb(bar)),
            round_tl,
            round_tr,
            false,
            false,
        )
        .when(!left_buttons.is_empty(), |el| {
            el.child(render_button_group(
                "caption-left",
                &left_buttons,
                maximized,
                to_tray,
                ink,
                dark,
                style,
            ))
        })
        .when_some(leading, |el, child| el.child(child))
        .child(
            div()
                .id("titleBar-drag")
                .window_control_area(gpui::WindowControlArea::Drag)
                .flex_1()
                .h_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .pl(px(12.))
                .text_size(px(13.))
                .text_color(rgb(ink))
                .on_mouse_down(MouseButton::Left, |_, window, _| {
                    window.start_window_move();
                })
                .when(with_zoom, |el| {
                    el.on_click(|event, window, _| {
                        if event.click_count() >= 2 {
                            window.zoom_window();
                        }
                    })
                })
                .when_some(title_icon, |el, icon| {
                    el.child(img(icon).size(px(18.)).rounded(px(4.)))
                })
                .child(title),
        )
        .when_some(trailing, |el, child| el.child(child))
        .when(!right_buttons.is_empty(), |el| {
            el.child(render_button_group(
                "caption-right",
                &right_buttons,
                maximized,
                to_tray,
                ink,
                dark,
                style,
            ))
        })
}

fn render_button_group(
    group_id: &'static str,
    buttons: &[WindowButton],
    maximized: bool,
    to_tray: bool,
    ink: u32,
    dark: bool,
    style: CaptionStyle,
) -> impl gpui::IntoElement {
    let gap = match style {
        CaptionStyle::MacTrafficLight => 8.,
        CaptionStyle::KdeBreeze => 4.,
        CaptionStyle::LinuxRound => 6.,
        CaptionStyle::Windows => 0.,
    };
    let pad_x = match style {
        CaptionStyle::MacTrafficLight => 10.,
        CaptionStyle::Windows => 0.,
        _ => 8.,
    };
    div()
        .id(group_id)
        .flex()
        .flex_row()
        .items_center()
        .when(style == CaptionStyle::Windows, |el| el.h_full())
        .gap(px(gap))
        .px(px(pad_x))
        .children(
            buttons
                .iter()
                .copied()
                .map(|btn| render_distro_caption_btn(btn, maximized, to_tray, ink, dark, style)),
        )
}

fn render_distro_caption_btn(
    button: WindowButton,
    maximized: bool,
    to_tray: bool,
    ink: u32,
    dark: bool,
    style: CaptionStyle,
) -> gpui::AnyElement {
    let (id, icon_kind, tip_text) = match button {
        WindowButton::Minimize => ("caption-min", CaptionIconKind::Minimize, "Thu nhỏ"),
        WindowButton::Maximize if maximized => {
            ("caption-max", CaptionIconKind::Restore, "Khôi phục kích thước")
        }
        WindowButton::Maximize => ("caption-max", CaptionIconKind::Maximize, "Phóng to"),
        WindowButton::Close => ("caption-close", CaptionIconKind::Close, "Đóng"),
    };

    let trigger = move |window: &mut Window, _: &mut App| match button {
        WindowButton::Minimize => window.minimize_window(),
        WindowButton::Maximize => window.zoom_window(),
        WindowButton::Close => {
            // Main window with a tray: stay running like Zalo PC. GPUI
            // 0.2.2 on Wayland stops the event loop when the last window
            // is dropped, so the window is minimized instead of removed.
            if to_tray && zalo_shell::tray_active() {
                window.minimize_window();
                crate::background::tell_tray_once();
            } else {
                window.remove_window();
            }
        }
    };

    if style == CaptionStyle::MacTrafficLight {
        let (dot_bg, dot_border) = match button {
            WindowButton::Close => (0xff_5f_57, 0xe0_44_3e),
            WindowButton::Minimize => (0xfe_bc_2e, 0xd8_9e_24),
            WindowButton::Maximize => (0x28_c8_40, 0x1a_ab_29),
        };
        return div()
            .id(id)
            .size(px(13.))
            .rounded(px(6.5))
            .bg(rgb(dot_bg))
            .border_1()
            .border_color(rgb(dot_border))
            .cursor_pointer()
            .hover(|s| s.opacity(0.84))
            .tooltip(move |_, cx| crate::widgets::tip(tip_text, cx))
            .on_click(move |_, window, cx| trigger(window, cx))
            .into_any_element();
    }

    let is_close = button == WindowButton::Close;

    if style == CaptionStyle::Windows {
        let hover_bg = if is_close {
            rgb(0xc4_2b_1c)
        } else if dark {
            rgb(0x3a_3e_46)
        } else {
            rgb(0xd4_d8_df)
        };
        let hover_fg = if is_close { 0xff_ff_ff } else { ink };
        return div()
            .id(id)
            .w(px(46.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_size(px(14.))
            .text_color(rgb(ink))
            .hover(move |s| s.bg(hover_bg).text_color(rgb(hover_fg)))
            .tooltip(move |_, cx| crate::widgets::tip(tip_text, cx))
            .on_click(move |_, window, cx| trigger(window, cx))
            .child(icon_kind.fallback_glyph())
            .into_any_element();
    }

    let btn_size = match style {
        CaptionStyle::KdeBreeze => 24.,
        _ => 26.,
    };
    let rest_bg = match style {
        CaptionStyle::KdeBreeze | CaptionStyle::Windows => hsla(0., 0., 0., 0.0),
        CaptionStyle::LinuxRound | CaptionStyle::MacTrafficLight => {
            if dark {
                hsla(0., 0., 1., 0.08)
            } else {
                hsla(0., 0., 0., 0.06)
            }
        }
    };
    let hover_bg = if is_close {
        if style == CaptionStyle::KdeBreeze {
            hsla(0.98, 0.65, 0.56, 0.90)
        } else {
            hsla(0.99, 0.78, 0.50, 0.20)
        }
    } else if dark {
        hsla(0., 0., 1., 0.16)
    } else {
        hsla(0., 0., 0., 0.13)
    };
    let icon_color = if is_close && style == CaptionStyle::LinuxRound {
        if dark { 0xee_f2_f6 } else { ink }
    } else {
        ink
    };
    let svg_img = caption_icon_image(icon_kind, icon_color);
    let has_svg = svg_img.is_some();

    div()
        .id(id)
        .size(px(btn_size))
        .rounded(px(btn_size * 0.5))
        .bg(rest_bg)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .tooltip(move |_, cx| crate::widgets::tip(tip_text, cx))
        .on_click(move |_, window, cx| trigger(window, cx))
        .when_some(svg_img, |el, bitmap| {
            el.child(
                img(bitmap)
                    .size(px(14.))
                    .object_fit(gpui::ObjectFit::Contain),
            )
        })
        .when(!has_svg, |el| {
            el.text_size(px(14.))
                .text_color(rgb(icon_color))
                .child(icon_kind.fallback_glyph())
        })
        .into_any_element()
}
