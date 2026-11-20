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
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(4.))
            .bg(rgb(0x081b3a))
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
