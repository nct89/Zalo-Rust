//! `.login-qr-page` — CSS boxes from the Zalo desktop UI spec.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gpui::{
    div, hsla, img, prelude::*, px, rgb, Context, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, Render, RenderImage, StatefulInteractiveElement, Styled, Timer, WeakEntity,
    Window,
};
use image::Frame;

use crate::color::str_vi;
use crate::qr::{rasterize_qr, PREVIEW_QR_PAYLOAD, QR_TIMEOUT_SECS};
use crate::widgets::tip;
use crate::window_frame::wrap_csd;

/// Root view of the 420×644 login window.
pub struct LoginView {
    qr: Option<Arc<RenderImage>>,
    expired: bool,
    generation: u64,
    /// Real `request_qr` flow (false in tests / `ZALO_GPUI_LOGIN_PREVIEW`).
    live: bool,
    status: Option<String>,
    scanned: Option<String>,
    error: Option<String>,
    menu_open: bool,
    english: bool,
}

impl LoginView {
    /// Preview QR (no live `request_qr`). Timeout overlay after `qr_timeout` 180 s.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        window.set_window_title("Zalo - Đăng nhập");
        cx.observe_window_appearance(window, |_, window, _| {
            window.refresh();
        })
        .detach();
        if !cfg!(test) {
            let weak = cx.weak_entity();
            window
                .spawn(cx, async move |cx| {
                    for delay_ms in [50u64, 180, 400] {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(delay_ms))
                            .await;
                        let _ = weak.update_in(cx, |_, window, cx| {
                            window.refresh();
                            cx.notify();
                        });
                    }
                })
                .detach();
        }
        let live = !cfg!(test) && std::env::var_os("ZALO_GPUI_LOGIN_PREVIEW").is_none();
        let mut view = Self {
            qr: if live {
                None
            } else {
                encode_qr_image(PREVIEW_QR_PAYLOAD)
            },
            expired: false,
            generation: 1,
            live,
            status: None,
            scanned: None,
            error: None,
            menu_open: false,
            english: false,
        };
        if live {
            view.start_flow(window, cx);
        } else {
            view.arm_timeout(window, cx);
        }
        view
    }

    /// One real QR attempt; events from older attempts are ignored.
    fn start_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use futures::StreamExt;
        let generation = self.generation;
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        crate::login_flow::start(tx);
        let this = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                while let Some(event) = rx.next().await {
                    let done = event == crate::login_flow::LoginEvent::Done;
                    let _ = this.update_in(cx, |view, window, cx| {
                        view.on_login_event(generation, event, window, cx);
                    });
                    if done {
                        break;
                    }
                }
            })
            .detach();
    }

    fn on_login_event(
        &mut self,
        generation: u64,
        event: crate::login_flow::LoginEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::login_flow::LoginEvent;
        if generation != self.generation {
            return;
        }
        match event {
            LoginEvent::Status(text) => self.status = Some(text),
            LoginEvent::Qr(prompt) => {
                self.qr = encode_qr_prompt(&prompt);
                self.expired = false;
                self.error = None;
                self.scanned = None;
                if prompt.security_step {
                    self.status = Some(
                        prompt
                            .message
                            .filter(|m| !m.trim().is_empty())
                            .map(|m| format!("Xác nhận bảo mật: {m} — Quét lại mã QR mới trên điện thoại."))
                            .unwrap_or_else(|| {
                                "Xác nhận bảo mật: Mở Zalo trên điện thoại và quét lại mã QR mới này."
                                    .into()
                            }),
                    );
                } else {
                    self.status = None;
                }
            }
            LoginEvent::Scanned(name) => {
                self.scanned = Some(name.unwrap_or_default());
                self.status = None;
            }
            LoginEvent::Expired => {
                self.expired = true;
                self.scanned = None;
            }
            LoginEvent::Failed(err) => {
                self.error = Some(err);
                self.scanned = None;
                self.expired = true;
            }
            LoginEvent::Done => {
                crate::open_shell_window(cx);
                window.remove_window();
            }
        }
        cx.notify();
    }

    fn arm_timeout(&self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.generation;
        let this = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                Timer::after(Duration::from_secs(QR_TIMEOUT_SECS)).await;
                expire_if_current(this, generation, cx);
            })
            .detach();
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.expired = false;
        self.scanned = None;
        self.error = None;
        if self.live {
            self.qr = None;
            self.start_flow(window, cx);
        } else {
            self.qr = encode_qr_image(PREVIEW_QR_PAYLOAD);
            self.arm_timeout(window, cx);
        }
        cx.notify();
    }

    fn hamburger(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = 0x08_1b_3a;
        let hover = 0xf1_f2_f4;
        let open = self.menu_open;
        div()
            .id("login-hamburger")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(4.))
            .w(px(32.))
            .h(px(28.))
            .rounded(px(6.))
            .cursor_pointer()
            .when(open, |el| el.bg(rgb(hover)))
            .hover(|style| style.bg(rgb(hover)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.menu_open = !this.menu_open;
                cx.notify();
            }))
            .children((0..3).map(move |_| div().w(px(18.)).h(px(1.8)).rounded(px(1.)).bg(rgb(ink))))
    }

    fn menu_dismiss(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("login-menu-dismiss")
            .absolute()
            .inset(px(0.))
            .on_click(cx.listener(|this, _, _, cx| {
                this.menu_open = false;
                cx.notify();
            }))
    }

    fn login_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paper = 0xff_ff_ff;
        let ink = 0x08_1b_3a;
        let muted = 0x5a_69_81;
        let hover = 0xf1_f2_f4;
        let line = hsla(0., 0., 0., 0.08);
        let lang = if self.english {
            "English"
        } else {
            "Tiếng Việt"
        };
        let rows: [(&'static str, bool, u8); 4] = [
            (
                str_vi(
                    "STR_LOGIN_PASSWORD_PAGE_MENU",
                    "Đăng nhập với số điện thoại",
                ),
                false,
                0,
            ),
            ("Đăng nhập với Facebook", false, 1),
            (str_vi("STR_MENU_LANGUAGE", "Ngôn ngữ"), true, 2),
            ("Quốc gia", false, 3),
        ];
        div()
            .id("login-hamburger-menu")
            .absolute()
            .top(px(44.))
            .right(px(12.))
            .w(px(240.))
            .py(px(8.))
            .rounded(px(8.))
            .bg(rgb(paper))
            .border_1()
            .border_color(line)
            .shadow_sm()
            .children(rows.into_iter().map(move |(label, enabled, kind)| {
                let value = match kind {
                    2 => Some(lang),
                    3 => Some("Việt Nam"),
                    _ => None,
                };
                let color = if enabled { ink } else { muted };
                let row = div()
                    .id(("login-menu", kind as u64))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(40.))
                    .px(px(16.))
                    .text_size(px(14.))
                    .text_color(rgb(color))
                    .when(enabled, |el| {
                        el.cursor_pointer().hover(|style| style.bg(rgb(hover)))
                    });
                let row = if enabled {
                    row.on_click(cx.listener(|this, _, _, cx| {
                        this.english = !this.english;
                        this.menu_open = false;
                        cx.notify();
                    }))
                } else {
                    row.tooltip(move |_, cx| tip("Bản Linux chưa hỗ trợ mục này", cx))
                };
                row.child(label).child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .when_some(value, |el, text| el.child(text)),
                )
            }))
    }
}

fn expire_if_current(
    this: WeakEntity<LoginView>,
    generation: u64,
    cx: &mut gpui::AsyncWindowContext,
) {
    let _ = this.update(cx, |view, cx| {
        if view.generation == generation {
            view.expired = true;
            cx.notify();
        }
    });
}

fn encode_qr_image(payload: &str) -> Option<Arc<RenderImage>> {
    if payload.is_empty() {
        return None;
    }
    let bitmap = rasterize_qr(payload, zalo_design::LOGIN_QR_IMAGE as u32).ok()?;
    Some(Arc::new(RenderImage::new([Frame::new(
        crate::media::rgba_to_bgra(bitmap),
    )])))
}

fn encode_qr_base64(raw_b64: &str) -> Option<Arc<RenderImage>> {
    use base64::Engine as _;
    let trimmed = raw_b64
        .trim()
        .strip_prefix("data:image/png;base64,")
        .or_else(|| raw_b64.trim().strip_prefix("data:image/jpeg;base64,"))
        .unwrap_or_else(|| raw_b64.trim());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .ok()?;
    let rgba = crate::media::decode_rgba(&bytes, zalo_design::LOGIN_QR_IMAGE as u32)?;
    Some(Arc::new(RenderImage::new([Frame::new(
        crate::media::rgba_to_bgra(rgba),
    )])))
}

fn encode_qr_prompt(prompt: &crate::login_flow::QrPrompt) -> Option<Arc<RenderImage>> {
    if !prompt.url.is_empty() {
        if let Some(img) = encode_qr_image(&prompt.url) {
            return Some(img);
        }
    }
    prompt
        .base64_png
        .as_deref()
        .and_then(encode_qr_base64)
}

impl Render for LoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Electron login window is always crisp white regardless of app Dark mode.
        let ink = 0x08_1b_3a;
        let muted = 0x5a_69_81;
        let blue = 0x00_68_ff;
        let field = 0xf1_f2_f4;
        let error = 0xc3_18_18;
        let expired = self.expired;
        let qr = self.qr.clone();

        let corners = crate::window_frame::CsdCorners::from_window(window);
        wrap_csd(
            window,
            corners
                .apply(
                    div()
                        .relative()
                        .flex()
                        .flex_col()
                        .size_full()
                        .overflow_hidden()
                        .bg(rgb(0xff_ff_ff)),
                    true,
                    true,
                    true,
                    true,
                )
                .text_color(rgb(ink))
                .font_family(crate::fonts::FAMILY_UI)
                .child(
                    // Seamless white top drag bar + top-right hamburger matching Electron 420×644.
                    div()
                        .id("login-top-bar")
                        .flex()
                        .flex_row()
                        .items_center()
                        .w_full()
                        .h(px(40.))
                        .px(px(10.))
                        .child(
                            div()
                                .id("login-drag-strip")
                                .flex_1()
                                .h_full()
                                .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                                    window.start_window_move();
                                }),
                        )
                        .child(self.hamburger(cx)),
                )
                .child(
                    div()
                        .id("login-qr-page")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .items_center()
                        .w_full()
                        .px(px(zalo_design::LOGIN_PAGE_PAD_X))
                        .pt(px(28.))
                        .pb(px(zalo_design::LOGIN_PAGE_PAD_Y))
                        .gap(px(20.))
                        .child(title_block(blue, ink))
                        .child(qr_scan(expired, qr, field, muted, blue, error, cx))
                        .child(login_status(
                            self.scanned.as_deref(),
                            self.status.as_deref(),
                            self.error.as_deref(),
                            muted,
                            blue,
                            error,
                        ))
                        .child(instructions(ink)),
                )
                .when(self.menu_open, |el| {
                    el.child(self.menu_dismiss(cx)).child(self.login_menu(cx))
                }),
        )
    }
}

/// One line under the QR: progress, "Scanned", or the error.
fn login_status(
    scanned: Option<&str>,
    status: Option<&str>,
    err: Option<&str>,
    muted: u32,
    blue: u32,
    error: u32,
) -> impl IntoElement {
    let (text, color) = if let Some(e) = err {
        (format!("Không đăng nhập được: {e}"), error)
    } else if let Some(name) = scanned {
        let who = if name.is_empty() {
            String::new()
        } else {
            format!(" bởi {name}")
        };
        (
            format!("Đã quét{who}. Bấm «Đăng nhập» trên điện thoại để xác nhận."),
            blue,
        )
    } else if let Some(s) = status {
        let c = if s.starts_with("Xác nhận bảo mật") {
            blue
        } else {
            muted
        };
        (s.to_owned(), c)
    } else {
        (String::new(), muted)
    };
    div()
        .max_w(px(360.))
        .text_size(px(13.))
        .line_height(px(18.))
        .text_center()
        .whitespace_normal()
        .text_color(rgb(color))
        .child(text)
}

fn zalo_logo_image() -> Option<Arc<RenderImage>> {
    static LOGO: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
    LOGO.get_or_init(|| {
        let rgba = crate::media::decode_rgba(include_bytes!("../assets/zalo-logo.png"), 312)?;
        Some(Arc::new(RenderImage::new([Frame::new(
            crate::media::rgba_to_bgra(rgba),
        )])))
    })
    .clone()
}

fn title_block(blue: u32, ink: u32) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(14.))
        .child(
            div()
                .h(px(34.))
                .flex()
                .items_center()
                .justify_center()
                .child(match zalo_logo_image() {
                    Some(logo) => img(logo)
                        .w(px(86.))
                        .h(px(32.))
                        .object_fit(ObjectFit::Contain)
                        .into_any_element(),
                    None => div()
                        .text_size(px(34.))
                        .font_family(crate::fonts::FAMILY_BOLD)
                        .text_color(rgb(blue))
                        .child("Zalo")
                        .into_any_element(),
                }),
        )
        .child(
            div()
                .h(px(zalo_design::LOGIN_TITLE_H))
                .flex()
                .items_center()
                .text_size(px(17.))
                .line_height(px(24.))
                .font_family(crate::fonts::FAMILY_SEMIBOLD)
                .text_color(rgb(ink))
                .child(str_vi("STR_LOGIN_QR_PAGE_TITLE", "Quét mã QR để đăng nhập")),
        )
}

fn qr_scan(
    expired: bool,
    qr: Option<Arc<RenderImage>>,
    field: u32,
    muted: u32,
    blue: u32,
    error: u32,
    cx: &mut Context<LoginView>,
) -> impl IntoElement {
    let frame = px(zalo_design::LOGIN_QR_FRAME);
    let clickable = expired || qr.is_none();
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(zalo_design::LOGIN_QR_SCAN_GAP))
        .child(qr_frame(
            expired, qr, field, muted, blue, frame, clickable, cx,
        ))
        .when(expired, |parent| {
            parent.child(
                div()
                    .h(px(zalo_design::LOGIN_QR_ERROR_H))
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .text_color(rgb(error))
                    .child(str_vi(
                        "STR_LOGIN_QR_PAGE_QR_TIMEOUT_ERROR",
                        "Mã QR đã hết hạn",
                    )),
            )
        })
}

fn qr_frame(
    expired: bool,
    qr: Option<Arc<RenderImage>>,
    field: u32,
    muted: u32,
    blue: u32,
    frame: gpui::Pixels,
    clickable: bool,
    cx: &mut Context<LoginView>,
) -> impl IntoElement {
    let inner = if expired {
        expired_mask(blue).into_any_element()
    } else if let Some(image) = qr {
        live_qr(image, muted, blue).into_any_element()
    } else {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(field))
            .text_size(px(16.))
            .text_color(rgb(muted))
            .child("Mã QR")
            .into_any_element()
    };

    let mut box_el = div()
        .id("login-qr-frame")
        .relative()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size(frame)
        .bg(rgb(0xffffff))
        .rounded(px(8.))
        .overflow_hidden()
        .child(inner);

    if clickable {
        box_el = box_el
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| {
                this.reload(window, cx);
            }));
    }
    box_el
}

fn live_qr(image: Arc<RenderImage>, _muted: u32, _blue: u32) -> impl IntoElement {
    let qr_edge = px(zalo_design::LOGIN_QR_IMAGE);
    div()
        .flex()
        .items_center()
        .justify_center()
        .size_full()
        .child(img(image).size(qr_edge))
}

fn expired_mask(blue: u32) -> impl IntoElement {
    let ink = 0x08_1b_3a;
    div()
        .id("login-qr-retry")
        .absolute()
        .inset(px(-8.))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .rounded(px(24.))
        .bg(gpui::hsla(0., 0., 1., 0.75))
        .child(div().text_size(px(22.)).text_color(rgb(blue)).child("↻"))
        .child(
            div()
                .text_size(px(13.))
                .text_color(rgb(ink))
                .child(str_vi("STR_LOGIN_QR_PAGE_RETRY_GET_QR", "Bấm để tải lại")),
        )
}

fn instructions(ink: u32) -> impl IntoElement {
    let line = px(zalo_design::LOGIN_INSTR_H);
    div()
        .flex()
        .flex_col()
        .items_start()
        .w_full()
        .max_w(px(zalo_design::LOGIN_WINDOW.0 - 64.0))
        .gap(px(zalo_design::LOGIN_INSTR_GAP))
        .text_size(px(14.))
        .line_height(line)
        .text_color(rgb(ink))
        .child(guide_line(&format!(
            "1.  {}",
            str_vi(
                "STR_LOGIN_QR_PAGE_GUIDE_1",
                "Mở ứng dụng Zalo trên điện thoại của bạn",
            )
        )))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_start()
                .h(line)
                .gap(px(6.))
                .child(guide_line(&format!(
                    "2.  {}",
                    str_vi("STR_LOGIN_QR_PAGE_GUIDE_2_1", "Chọn biểu tượng")
                )))
                .child(
                    div()
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(ink))
                        .child(crate::fonts::icon_el('\u{ECE5}', "⊞", 18.)),
                )
                .child(guide_line(str_vi(
                    "STR_LOGIN_QR_PAGE_GUIDE_2_2",
                    "trên thanh tìm kiếm",
                ))),
        )
        .child(guide_line(&format!(
            "3.  {}",
            str_vi(
                "STR_LOGIN_QR_PAGE_GUIDE_3",
                "Quét mã QR để đăng nhập trên máy tính",
            )
        )))
}

/// One line. GPUI 0.2.2 treats Vietnamese letters outside basic Latin as
/// break points, so these captions must not wrap.
fn guide_line(text: &str) -> impl IntoElement {
    div().whitespace_nowrap().child(text.to_owned())
}
