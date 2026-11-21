//! Small GPUI chrome helpers (tooltip). No live APIs.

use gpui::{
    div, px, rgb, AnyView, App, AppContext, Context, IntoElement, ParentElement, Render,
    SharedString, Styled, Window,
};

/// Hover tooltip painted from a `STR_*` string.
pub(crate) struct Tip {
    text: SharedString,
}

impl Render for Tip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let dark = crate::color::is_dark();
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(4.))
            .bg(rgb(if dark { 0x18_1c_22 } else { 0x08_1b_3a }))
            .border_1()
            .border_color(if dark {
                gpui::hsla(0., 0., 1., 0.14)
            } else {
                gpui::transparent_black()
            })
            .text_color(rgb(0xffffff))
            .text_size(px(12.))
            .child(self.text.clone())
    }
}

/// Build a tooltip view for `div().tooltip(...)`.
pub(crate) fn tip(text: impl Into<SharedString>, cx: &mut App) -> AnyView {
    let text: SharedString = crate::i18n::t(&text.into()).into();
    cx.new(|_| Tip { text }).into()
}
