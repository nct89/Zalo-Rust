//! Client-side decorations: transparent surface + 8px inset + rounded quads
//! on the same elements that paint the four window corners.
//!
//! GPUI `overflow_hidden` clips to an AABB, so a parent `.rounded()` does not
//! punch through opaque children. Corner widgets (rail, titlebar, composer)
//! must carry matching `rounded_*` on the same node as their `.bg()`.

use gpui::{
    div, prelude::*, px, rgb, size, transparent_black, CursorStyle, Decorations,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, ResizeEdge, Size,
    Styled, Tiling, Window, WindowBackgroundAppearance,
};

use crate::color::token_rgb;

/// Which window corners currently paint a radius, and which edges keep the
/// 8px CSD inset. Maximized / fullscreen / tiled edges drop both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CsdCorners {
    /// Top-left radius active.
    pub tl: bool,
    /// Top-right radius active.
    pub tr: bool,
    /// Bottom-left radius active.
    pub bl: bool,
    /// Bottom-right radius active.
    pub br: bool,
    /// Pad the top edge (shadow inset).
    pub pad_top: bool,
    /// Pad the right edge.
    pub pad_right: bool,
    /// Pad the bottom edge.
    pub pad_bottom: bool,
    /// Pad the left edge.
    pub pad_left: bool,
    /// CSS `--card` / `WINDOW_RADIUS`, or 0 when every corner is off.
    pub radius: Pixels,
}

impl CsdCorners {
    /// No rounding, no inset — maximized / fullscreen.
    pub fn none() -> Self {
        Self {
            tl: false,
            tr: false,
            bl: false,
            br: false,
            pad_top: false,
            pad_right: false,
            pad_bottom: false,
            pad_left: false,
            radius: px(0.),
        }
    }

    /// Floating window: every corner + every edge.
    pub fn floating() -> Self {
        Self {
            tl: true,
            tr: true,
            bl: true,
            br: true,
            pad_top: true,
            pad_right: true,
            pad_bottom: true,
            pad_left: true,
            radius: px(zalo_design::LINUX_FRAME_RADIUS),
        }
    }

    /// Derive from tiling + zoom. `Decorations::Server` still rounds: KWin can
    /// report SSD while we draw the Linux `#titleBar` ourselves.
    pub fn from_tiling(tiling: Tiling, maximized: bool, fullscreen: bool) -> Self {
        if maximized || fullscreen {
            return Self::none();
        }
        if !tiling.is_tiled() {
            return Self::floating();
        }
        Self {
            tl: !(tiling.top || tiling.left),
            tr: !(tiling.top || tiling.right),
            bl: !(tiling.bottom || tiling.left),
            br: !(tiling.bottom || tiling.right),
            pad_top: !tiling.top,
            pad_right: !tiling.right,
            pad_bottom: !tiling.bottom,
            pad_left: !tiling.left,
            radius: px(zalo_design::LINUX_FRAME_RADIUS),
        }
    }

    /// Live window: Server decorations count as a floating CSD frame on
    /// composited desktops, and switch cleanly to crisp square corners (keeping
    /// resize pads) on non-composited X11 (e.g. `xfwm4 --compositor=off`).
    pub fn from_window(window: &Window) -> Self {
        // On Windows, DWM renders native drop shadows, Win11 rounded corners,
        // and non-client edge resize zones; adding an 8px transparent CSD pad
        // would leave an empty border inside the native HWND frame.
        #[cfg(target_os = "windows")]
        {
            let _ = window;
            return Self::none();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let tiling = match window.window_decorations() {
                Decorations::Client { tiling } => tiling,
                Decorations::Server => Tiling::default(),
            };
            let mut corners =
                Self::from_tiling(tiling, window.is_maximized(), window.is_fullscreen());
            if !crate::x11_frame::is_composited() {
                corners.tl = false;
                corners.tr = false;
                corners.bl = false;
                corners.br = false;
                corners.radius = px(0.);
            }
            corners
        }
    }

    /// Any edge currently padded.
    pub fn any_pad(self) -> bool {
        self.pad_top || self.pad_right || self.pad_bottom || self.pad_left
    }

    /// Apply selected radii. Each flag is AND-ed with the live CSD corner.
    pub fn apply<E: Styled>(self, el: E, tl: bool, tr: bool, bl: bool, br: bool) -> E {
        let mut el = el;
        if self.tl && tl {
            el = el.rounded_tl(self.radius);
        }
        if self.tr && tr {
            el = el.rounded_tr(self.radius);
        }
        if self.bl && bl {
            el = el.rounded_bl(self.radius);
        }
        if self.br && br {
            el = el.rounded_br(self.radius);
        }
        el
    }
}

/// Wrap window content in a transparent CSD frame.
///
/// Always pads and rounds unless maximized / fullscreen / tiled. A Server
/// decoration report used to skip this path, which left a square opaque fill.
pub fn wrap_csd(window: &mut Window, content: impl IntoElement) -> gpui::AnyElement {
    // XFCE (and any WM without `_GTK_FRAME_EXTENTS`) would keep a square
    // server frame. Drop the OS frame here; the ARGB buffer rounds the
    // corners. Wayland returns immediately.
    let vp = window.viewport_size();
    let frame_changed = crate::x11_frame::sync_window(
        f32::from(vp.width).round() as i32,
        f32::from(vp.height).round() as i32,
        window.is_maximized(),
        window.scale_factor(),
    );
    if frame_changed {
        window.refresh();
    }
    let composited = crate::x11_frame::is_composited();
    let corners = CsdCorners::from_window(window);
    let paper = token_rgb("--layer-background", "#ffffff");
    let show_hairline = !window.is_maximized() && !window.is_fullscreen();
    // Apply client inset and background appearance once per window (and again
    // if compositing state changes, or right after `_MOTIF_WM_HINTS` strips a
    // server frame so Blade/Vulkan reconfigures its swapchain onto xfwm4's
    // newly redirected XComposite pixmap instead of staying black).
    {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        static APPLIED: OnceLock<Mutex<HashMap<gpui::WindowId, bool>>> = OnceLock::new();
        let wid = window.window_handle().window_id();
        let prev = APPLIED
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .ok()
            .and_then(|mut map| map.insert(wid, composited));
        if prev.is_none() {
            window.set_client_inset(px(0.));
        }
        if frame_changed && composited {
            window.set_background_appearance(WindowBackgroundAppearance::Opaque);
            window.set_background_appearance(WindowBackgroundAppearance::Transparent);
            window.refresh();
        } else if prev != Some(composited) {
            window.set_background_appearance(if composited {
                WindowBackgroundAppearance::Transparent
            } else {
                WindowBackgroundAppearance::Opaque
            });
        }
    }

    // On composited desktops keep `#window-chrome` transparent so corner
    // widgets (e.g. blue `#main-tab` rail) anti-alias directly against the
    // desktop with zero white-plate fringing. On non-composited X11 paint solid
    // `paper` with 0 px corners so no premultiplied-black crumbs appear.
    let chrome = corners.apply(
        div()
            .id("window-chrome")
            .relative()
            .size_full()
            .overflow_hidden()
            .when(composited, |el| el.bg(transparent_black()))
            .when(!composited, |el| el.bg(rgb(paper)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(content)
            .when(show_hairline, |el| el.child(frame_hairline(corners))),
        true,
        true,
        true,
        true,
    );

    // 12 px grips sit on top of the 8 px shadow pad *and* a few pixels of
    // chrome. The 8 px pad alone is too thin on a 1.35 scale Wayland screen.
    const GRIP: f32 = 12.0;
    div()
        .id("window-backdrop")
        .relative()
        .bg(transparent_black())
        .size_full()
        .on_mouse_down(MouseButton::Left, move |event, window, _| {
            let size = window.window_bounds().get_bounds().size;
            if let Some(edge) = resize_edge(event.position, px(GRIP), size) {
                window.start_window_resize(edge);
            }
        })
        .child(chrome)
        .when(corners.pad_left, |el| {
            el.child(resize_strip(
                "resize-w",
                CursorStyle::ResizeLeftRight,
                ResizeEdge::Left,
                true,
                false,
            ))
        })
        .when(corners.pad_right, |el| {
            el.child(resize_strip(
                "resize-e",
                CursorStyle::ResizeLeftRight,
                ResizeEdge::Right,
                true,
                false,
            ))
        })
        .when(corners.pad_top, |el| {
            el.child(resize_strip(
                "resize-n",
                CursorStyle::ResizeUpDown,
                ResizeEdge::Top,
                false,
                true,
            ))
        })
        .when(corners.pad_bottom, |el| {
            el.child(resize_strip(
                "resize-s",
                CursorStyle::ResizeUpDown,
                ResizeEdge::Bottom,
                false,
                true,
            ))
        })
        .when(corners.pad_top && corners.pad_left, |el| {
            el.child(resize_corner(
                "resize-nw",
                CursorStyle::ResizeUpLeftDownRight,
                ResizeEdge::TopLeft,
                true,
                true,
            ))
        })
        .when(corners.pad_top && corners.pad_right, |el| {
            el.child(resize_corner(
                "resize-ne",
                CursorStyle::ResizeUpRightDownLeft,
                ResizeEdge::TopRight,
                false,
                true,
            ))
        })
        .when(corners.pad_bottom && corners.pad_left, |el| {
            el.child(resize_corner(
                "resize-sw",
                CursorStyle::ResizeUpRightDownLeft,
                ResizeEdge::BottomLeft,
                true,
                false,
            ))
        })
        .when(corners.pad_bottom && corners.pad_right, |el| {
            el.child(resize_corner(
                "resize-se",
                CursorStyle::ResizeUpLeftDownRight,
                ResizeEdge::BottomRight,
                false,
                false,
            ))
        })
        .into_any_element()
}

/// Continuous 1 px outer hairline aligned flush with the window edge and curve.
/// Light uses black 14%, dark uses white 14% (matching `preload-child-linux.js`).
fn frame_hairline(corners: CsdCorners) -> gpui::AnyElement {
    let color = if crate::color::is_dark() {
        gpui::hsla(0., 0., 1., 0.14)
    } else {
        gpui::hsla(0., 0., 0., 0.14)
    };
    corners
        .apply(
            div()
                .absolute()
                .inset_0()
                .border_1()
                .border_color(color),
            true,
            true,
            true,
            true,
        )
        .into_any_element()
}

fn resize_strip(
    id: &'static str,
    cursor: CursorStyle,
    edge: ResizeEdge,
    vertical: bool,
    horizontal: bool,
) -> impl IntoElement {
    let g = px(12.);
    div()
        .id(id)
        .absolute()
        .when(vertical, |el| el.top_0().bottom_0().w(g))
        .when(horizontal, |el| el.left_0().right_0().h(g))
        .when(matches!(edge, ResizeEdge::Left), |el| el.left_0())
        .when(matches!(edge, ResizeEdge::Right), |el| el.right_0())
        .when(matches!(edge, ResizeEdge::Top), |el| el.top_0())
        .when(matches!(edge, ResizeEdge::Bottom), |el| el.bottom_0())
        .cursor(cursor)
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.start_window_resize(edge);
            cx.stop_propagation();
        })
}

fn resize_corner(
    id: &'static str,
    cursor: CursorStyle,
    edge: ResizeEdge,
    left: bool,
    top: bool,
) -> impl IntoElement {
    let g = px(16.);
    div()
        .id(id)
        .absolute()
        .w(g)
        .h(g)
        .when(left, |el| el.left_0())
        .when(!left, |el| el.right_0())
        .when(top, |el| el.top_0())
        .when(!top, |el| el.bottom_0())
        .cursor(cursor)
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.start_window_resize(edge);
            cx.stop_propagation();
        })
}

fn resize_edge(pos: Point<Pixels>, shadow: Pixels, size: Size<Pixels>) -> Option<ResizeEdge> {
    let near_top = pos.y < shadow;
    let near_bottom = pos.y > size.height - shadow;
    let near_left = pos.x < shadow;
    let near_right = pos.x > size.width - shadow;
    Some(match (near_top, near_bottom, near_left, near_right) {
        (true, _, true, _) => ResizeEdge::TopLeft,
        (true, _, _, true) => ResizeEdge::TopRight,
        (true, _, _, _) => ResizeEdge::Top,
        (_, true, true, _) => ResizeEdge::BottomLeft,
        (_, true, _, true) => ResizeEdge::BottomRight,
        (_, true, _, _) => ResizeEdge::Bottom,
        (_, _, true, _) => ResizeEdge::Left,
        (_, _, _, true) => ResizeEdge::Right,
        _ => return None,
    })
}

/// Outer window size = CSS content + CSD shadow on every edge (zero pad on Windows DWM).
pub fn outer_size(width: f32, height: f32) -> Size<Pixels> {
    #[cfg(target_os = "windows")]
    {
        size(px(width), px(height))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let pad = zalo_design::WINDOW_SHADOW * 2.0;
        size(px(width + pad), px(height + pad))
    }
}

/// Convert a window-space click into chrome-local pixels (skip CSD inset).
pub fn window_to_chrome(window: &Window, pos: Point<Pixels>) -> (f32, f32) {
    let corners = CsdCorners::from_window(window);
    let shadow = zalo_design::WINDOW_SHADOW;
    let dx = if corners.pad_left { shadow } else { 0.0 };
    let dy = if corners.pad_top { shadow } else { 0.0 };
    (f32::from(pos.x) - dx, f32::from(pos.y) - dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    #[test]
    fn csd_uses_card_radius_and_shadow() {
        assert_eq!(zalo_design::WINDOW_RADIUS, 8.0);
        assert_eq!(zalo_design::WINDOW_SHADOW, 8.0);
        assert_eq!(zalo_design::WINDOW_RADIUS, zalo_design::CARD_RADIUS);
        let size = outer_size(1000.0, 700.0);
        assert_eq!(f32::from(size.width), 1016.0);
        assert_eq!(f32::from(size.height), 716.0);
    }

    #[test]
    fn floating_csd_rounds_all_corners() {
        let c = CsdCorners::floating();
        assert!(c.tl && c.tr && c.bl && c.br);
        assert!(c.pad_top && c.pad_right && c.pad_bottom && c.pad_left);
        assert!(c.any_pad());
        assert_eq!(f32::from(c.radius), zalo_design::LINUX_FRAME_RADIUS);
    }

    #[test]
    fn maximized_csd_skips_rounding() {
        let c = CsdCorners::from_tiling(Tiling::default(), true, false);
        assert_eq!(c, CsdCorners::none());
        assert!(!c.any_pad());
        assert_eq!(f32::from(c.radius), 0.0);
    }

    #[test]
    fn server_decorations_still_round_like_floating() {
        let c = CsdCorners::from_tiling(Tiling::default(), false, false);
        assert_eq!(c, CsdCorners::floating());
    }

    #[test]
    fn resize_grip_12px_hits_left_and_right() {
        let size = size(px(400.), px(300.));
        let g = px(12.);
        assert_eq!(
            resize_edge(point(px(4.), px(150.)), g, size),
            Some(ResizeEdge::Left)
        );
        assert_eq!(
            resize_edge(point(px(396.), px(150.)), g, size),
            Some(ResizeEdge::Right)
        );
        assert_eq!(resize_edge(point(px(200.), px(150.)), g, size), None);
        assert_eq!(
            resize_edge(point(px(4.), px(4.)), g, size),
            Some(ResizeEdge::TopLeft)
        );
    }

    #[test]
    fn tiled_top_skips_top_corners() {
        let c = CsdCorners::from_tiling(
            Tiling {
                top: true,
                ..Tiling::default()
            },
            false,
            false,
        );
        assert!(!c.tl && !c.tr);
        assert!(c.bl && c.br);
        assert!(!c.pad_top);
        assert!(c.pad_bottom && c.pad_left && c.pad_right);
    }
}
