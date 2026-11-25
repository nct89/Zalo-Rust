//! Small GPUI chrome helpers (tooltip). No live APIs.

use gpui::{
    div, px, rgb, AnyView, App, AppContext, Context, IntoElement, ParentElement, Render,
    SharedString, Styled, Window,
};

/// Hover tooltip painted from a `STR_*` string, following Zalo PC `.z-tooltip`
/// (`max-width: 250px; word-wrap: break-word;`) so long tooltips wrap cleanly
/// instead of overflowing across columns or past window bounds.
pub(crate) struct Tip {
    text: SharedString,
}

const TOOLTIP_MAX_W: f32 = 250.0;
const TOOLTIP_PAD_X: f32 = 8.0;

impl Render for Tip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let dark = crate::color::is_dark();
        let wrapped =
            crate::media::wrap_text_at(&self.text, TOOLTIP_MAX_W - TOOLTIP_PAD_X * 2.0, 12.0, false);
        let lines: Vec<String> = wrapped.lines().map(str::to_owned).collect();
        div()
            .flex()
            .flex_col()
            .max_w(px(TOOLTIP_MAX_W))
            .px(px(TOOLTIP_PAD_X))
            .py(px(4.))
            .rounded(px(4.))
            .bg(rgb(if dark { 0x1a_1a_1a } else { 0x08_1b_3a }))
            .border_1()
            .border_color(if dark {
                gpui::hsla(0., 0., 1., 0.14)
            } else {
                gpui::transparent_black()
            })
            .text_color(rgb(0xffffff))
            .text_size(px(12.))
            .line_height(px(17.))
            .children(lines.into_iter().map(|line| div().child(line)))
    }
}

/// Build a tooltip view for `div().tooltip(...)`.
pub(crate) fn tip(text: impl Into<SharedString>, cx: &mut App) -> AnyView {
    let text: SharedString = crate::i18n::t(&text.into()).into();
    cx.new(|_| Tip { text }).into()
}
