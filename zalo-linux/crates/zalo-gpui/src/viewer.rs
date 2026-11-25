//! In-conversation photo viewer, laid out like Zalo PC's `media-viewer`.
//!
//! A solid dark layer covers the shell (the previous overlay shrank to the
//! toolbar, so the page underneath stayed visible). The header shows the
//! sender, time and `n / total`. The footer has zoom, fit, actual size,
//! rotate, save and copy. Side arrows and the left/right keys walk every
//! photo and GIF already loaded in this conversation. `zoom` 1 is fit;
//! 100% is one image pixel per logical pixel. The wheel zooms. Drag pans
//! once the bitmap is larger than the stage. Escape still closes.

use std::sync::Arc;

use gpui::{
    div, hsla, img, prelude::*, px, rgb, Context, CursorStyle, InteractiveElement, IntoElement,
    ParentElement, RenderImage, ScrollWheelEvent, StatefulInteractiveElement, Styled, Window,
};
use image::RgbaImage;

use crate::data::ChatMsg;
use crate::fonts::FAMILY_SEMIBOLD;
use crate::media::MsgKind;
use crate::shell::{ShellView, Viewer};

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 6.0;
/// Electron popup viewer body (`rgba(58, 60, 61)`).
const VIEWER_BG: u32 = 0x3a3c3d;

impl ShellView {
    fn set_zoom(&mut self, zoom: f32) {
        if let Some(v) = self.viewer.as_mut() {
            v.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
            if (v.zoom - 1.0).abs() < 0.02 {
                v.pan_x = 0.0;
                v.pan_y = 0.0;
            }
        }
    }

    /// Visible pan, also written back so the next drag starts where the photo is.
    fn clamped_viewer_pan(&mut self, window: &Window) -> (f32, f32) {
        let (vw, vh) = viewer_box(window);
        let fit = self.viewer_fit(window);
        let Some(v) = self.viewer.as_mut() else {
            return (0.0, 0.0);
        };
        let (pw, ph) = viewer_pixels(v);
        let (px, py) = clamp_viewer_pan(
            v.pan_x,
            v.pan_y,
            (pw * fit * v.zoom).max(1.0),
            (ph * fit * v.zoom).max(1.0),
            vw,
            (vh - 48.0 - 64.0).max(80.0),
        );
        v.pan_x = px;
        v.pan_y = py;
        (px, py)
    }

    /// Decode the open photo again. A newer photo or a newer rotation drops
    /// the result, so a slow file cannot paint over the one on screen.
    pub(crate) fn reload_viewer(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.viewer.as_ref() else {
            return;
        };
        let id = v.msg.id;
        let turns = v.turns % 4;
        let path = v.msg.media_path.clone().filter(|p| p.is_file());
        let url = v.msg.media_url.clone();
        cx.spawn(async move |this, cx| {
            let image = cx
                .background_executor()
                .spawn(async move {
                    let file = path
                        .or_else(|| crate::media::cache_path_for(&url).filter(|p| p.is_file()))?;
                    let bytes = std::fs::read(file).ok()?;
                    let rgba = crate::media::decode_rgba(&bytes, 2400)?;
                    let rgba = rotate_turns(rgba, turns);
                    Some(Arc::new(RenderImage::new([image::Frame::new(
                        crate::media::rgba_to_bgra(rgba),
                    )])))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(v) = view.viewer.as_mut() {
                    if v.msg.id == id && v.turns % 4 == turns {
                        v.image = image;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Photos and GIFs of the open conversation, oldest first. The current
    /// bubble is appended when it is not in the loaded window (info panel).
    fn viewer_gallery(&self) -> (usize, Vec<ChatMsg>) {
        let Some(v) = self.viewer.as_ref() else {
            return (0, Vec::new());
        };
        let mut items: Vec<ChatMsg> = self
            .messages
            .get(&v.msg.conv)
            .map(|msgs| {
                msgs.iter()
                    .filter(|m| {
                        !m.recalled
                            && matches!(m.kind, MsgKind::Photo | MsgKind::Doodle | MsgKind::Gif)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let idx = items
            .iter()
            .position(|m| m.id == v.msg.id)
            .unwrap_or_else(|| {
                items.push(v.msg.clone());
                items.len() - 1
            });
        (idx, items)
    }

    fn viewer_step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let (idx, items) = self.viewer_gallery();
        if items.is_empty() {
            return;
        }
        let next = idx as isize + delta;
        if next < 0 || next >= items.len() as isize {
            return;
        }
        let msg = items[next as usize].clone();
        self.open_viewer(msg, window, cx);
    }

    /// Rotate current image clockwise by `steps` quarter-turns and reload viewer bitmap.
    fn viewer_rotate(&mut self, steps: i8, cx: &mut Context<Self>) {
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        v.turns = (i16::from(v.turns) + i16::from(steps)).rem_euclid(4) as u8;
        v.pan_x = 0.0;
        v.pan_y = 0.0;
        v.image = None;
        v.ocr_result = None;
        self.reload_viewer(cx);
    }

    /// "Scan Text" opens the photo popup and copies the recognized text.
    pub(crate) fn run_viewer_ocr(
        &mut self,
        msg: ChatMsg,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let _ = window;
        self.open_photo(msg, true, cx);
    }

    fn viewer_fit(&self, window: &Window) -> f32 {
        let Some(v) = self.viewer.as_ref() else {
            return 1.0;
        };
        let (vw, vh) = viewer_box(window);
        let (w, h) = viewer_pixels(v);
        let avail_w = (vw - 96.0).max(80.0);
        let avail_h = (vh - 48.0 - 64.0).max(80.0);
        (avail_w / w).min(avail_h / h).clamp(0.02, 32.0)
    }

    fn viewer_set_actual(&mut self, window: &Window) {
        let fit = self.viewer_fit(window);
        self.set_zoom((1.0 / fit).clamp(ZOOM_MIN, ZOOM_MAX));
    }

    fn viewer_toggle_actual(&mut self, window: &Window) {
        let fit = self.viewer_fit(window);
        let actual = (1.0 / fit).clamp(ZOOM_MIN, ZOOM_MAX);
        let zoom = self.viewer.as_ref().map(|v| v.zoom).unwrap_or(1.0);
        self.set_zoom(if (zoom - actual).abs() < 0.05 {
            1.0
        } else {
            actual
        });
    }

    /// Keys while the viewer is open. Escape is left to `escape_overlay`.
    fn viewer_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.viewer.is_none() {
            return false;
        }
        match key {
            "left" => self.viewer_step(-1, window, cx),
            "right" => self.viewer_step(1, window, cx),
            "-" | "minus" => {
                let z = self.viewer.as_ref().map(|v| v.zoom).unwrap_or(1.0);
                self.set_zoom(z / 1.25);
            }
            "=" | "+" | "equal" => {
                let z = self.viewer.as_ref().map(|v| v.zoom).unwrap_or(1.0);
                self.set_zoom(z * 1.25);
            }
            "0" => self.set_zoom(1.0),
            "1" => self.viewer_set_actual(window),
            "r" => self.viewer_rotate(1, cx),
            _ => return false,
        }
        true
    }

    /// Shell key handler: viewer arrows and zoom, then lock and Escape.
    pub(crate) fn on_shell_key(
        &mut self,
        e: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mods = &e.keystroke.modifiers;
        let typing = mods.control || mods.alt || mods.platform || mods.function;
        if self.viewer.is_some()
            && !self.locked
            && !typing
            && self.viewer_key(&e.keystroke.key, window, cx)
        {
            cx.stop_propagation();
            cx.notify();
        } else if e.keystroke.key.eq_ignore_ascii_case("c")
            && (mods.control || mods.platform)
            && !mods.alt
            && !self.locked
            && self.viewer.is_none()
        {
            if let Some(selected) = self.selected_chat_text() {
                crate::clip::copy_text(cx, selected);
                self.set_note(crate::color::str_vi("STR_COPY_0", "Sao chép"));
                cx.stop_propagation();
                cx.notify();
            }
        } else if e.keystroke.key == "l" && mods.control && !self.locked {
            self.lock_now(window, cx);
            cx.stop_propagation();
            cx.notify();
        } else if e.keystroke.key == "escape" && !self.locked && self.escape_overlay() {
            cx.stop_propagation();
            cx.notify();
        } else if !self.locked {
            let (with, without) = self.shot_triggers();
            let slot = if crate::shortcuts::matches_keystroke(&without, e) {
                Some(1)
            } else if crate::shortcuts::matches_keystroke(&with, e) {
                Some(0)
            } else {
                None
            };
            if let Some(slot) = slot {
                self.trigger_shot_slot(slot, Some(window.window_handle()), cx);
                cx.stop_propagation();
                cx.notify();
            }
        }
    }

    pub(crate) fn viewer_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.viewer.is_none() {
            return div().into_any_element();
        }
        let (vw, vh) = viewer_box(window);
        let (idx, items) = self.viewer_gallery();
        let fit = self.viewer_fit(window);
        let v = self.viewer.as_ref().unwrap();
        let zoom = v.zoom;
        let (pw, ph) = viewer_pixels(v);
        let (dw, dh) = ((pw * fit * zoom).max(1.0), (ph * fit * zoom).max(1.0));
        let percent = (fit * zoom * 100.0).round().clamp(1.0, 9999.0);
        let total = items.len().max(1);
        let has_prev = idx > 0;
        let has_next = idx + 1 < items.len();
        let counter = format!("{} / {total}", idx + 1);
        let msg = v.msg.clone();
        let image = v.image.clone();
        let ocr_busy = v.ocr_busy;
        let stage_w = vw;
        let stage_h = (vh - 48.0 - 64.0).max(80.0);
        let (pan_x, pan_y) = clamp_viewer_pan(v.pan_x, v.pan_y, dw, dh, stage_w, stage_h);
        let ocr_result = v.ocr_result.clone();
        let can_ocr = matches!(msg.kind, MsgKind::Photo | MsgKind::Doodle | MsgKind::Gif);
        let caption =
            (!crate::media::hide_marker_text(msg.kind, &msg.text)).then(|| msg.text.clone());
        let img_left = (stage_w - dw) * 0.5 + pan_x;
        let img_top = (stage_h - dh) * 0.5 + pan_y;
        let can_pan = dw > stage_w + 1.0 || dh > stage_h + 1.0;
        let dragging = self.viewer_drag.is_some();
        let btn = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px(px(12.))
                .h(px(30.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(13.))
                .text_color(rgb(0xffffff))
                .bg(hsla(0., 0., 1., 0.12))
                .hover(|st| st.bg(hsla(0., 0., 1., 0.24)))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(label)
        };
        let save = msg.clone();
        let copy = msg.clone();
        let ocr_msg = msg.clone();
        let arrow = |id: &'static str, glyph: &'static str, left: bool| {
            div()
                .id(id)
                .absolute()
                .top(px(0.))
                .bottom(px(0.))
                .when(left, |el| el.left(px(8.)))
                .when(!left, |el| el.right(px(8.)))
                .w(px(44.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(
                    div()
                        .w(px(40.))
                        .h(px(64.))
                        .rounded(px(8.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(28.))
                        .text_color(rgb(0xffffff))
                        .bg(hsla(0., 0., 0., 0.35))
                        .child(glyph),
                )
        };
        div()
            .id("viewer-layer")
            .absolute()
            .top_0()
            .left_0()
            .w(px(vw))
            .h(px(vh))
            .occlude()
            .bg(rgb(VIEWER_BG))
            .on_click(cx.listener(|this, _, _, cx| {
                if this.viewer_dragged {
                    this.viewer_dragged = false;
                    this.viewer_drag = None;
                    return;
                }
                this.viewer = None;
                cx.notify();
            }))
            .child(
                div()
                    .id("viewer-column")
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(vw))
                    .h(px(vh))
                    .flex()
                    .flex_col()
                    .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                        cx.stop_propagation();
                        let dy = f32::from(e.delta.pixel_delta(px(20.)).y);
                        if dy == 0.0 {
                            return;
                        }
                        let z = this.viewer.as_ref().map(|v| v.zoom).unwrap_or(1.0);
                        this.set_zoom(if dy > 0.0 { z * 1.1 } else { z / 1.1 });
                        cx.notify();
                    }))
                    .child(
                        div()
                            .id("viewer-bar")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .h(px(48.))
                            .px(px(16.))
                            .bg(rgb(0x2e3031))
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .text_color(rgb(0xffffff))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .child(msg.sender.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(hsla(0., 0., 1., 0.7))
                                            .child(msg.time.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(hsla(0., 0., 1., 0.85))
                                    .child(counter),
                            )
                            .child(btn("viewer-close", "Đóng").on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.viewer = None;
                                cx.notify();
                            }))),
                    )
                    .child(
                        div()
                            .id("viewer-stage")
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_hidden()
                            .when(can_pan && !dragging, |el| el.cursor(CursorStyle::OpenHand))
                            .when(dragging, |el| el.cursor(CursorStyle::ClosedHand))
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener({
                                    let right_msg = msg.clone();
                                    move |this, e: &gpui::MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_menu(
                                            crate::context_menu::MenuKind::Message(
                                                right_msg.clone(),
                                            ),
                                            e.position,
                                            window,
                                        );
                                        cx.notify();
                                    }
                                }),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.viewer_dragged = false;
                                    let (px, py) = this.clamped_viewer_pan(window);
                                    this.viewer_drag = Some((
                                        f32::from(e.position.x),
                                        f32::from(e.position.y),
                                        px,
                                        py,
                                    ));
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, window, cx| {
                                let Some((ox, oy, px, py)) = this.viewer_drag else {
                                    return;
                                };
                                let dx = f32::from(e.position.x) - ox;
                                let dy = f32::from(e.position.y) - oy;
                                if dx.abs() + dy.abs() > 3.0 {
                                    this.viewer_dragged = true;
                                }
                                let (vw, vh) = viewer_box(window);
                                let fit = this.viewer_fit(window);
                                if let Some(v) = this.viewer.as_mut() {
                                    let (pw, ph) = viewer_pixels(v);
                                    let dw = (pw * fit * v.zoom).max(1.0);
                                    let dh = (ph * fit * v.zoom).max(1.0);
                                    let (cxp, cyp) = clamp_viewer_pan(
                                        px + dx,
                                        py + dy,
                                        dw,
                                        dh,
                                        vw,
                                        (vh - 48.0 - 64.0).max(80.0),
                                    );
                                    v.pan_x = cxp;
                                    v.pan_y = cyp;
                                }
                                cx.notify();
                            }))
                            .on_mouse_up(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _: &gpui::MouseUpEvent, _, _| {
                                    this.viewer_drag = None;
                                }),
                            )
                            .on_mouse_up_out(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _: &gpui::MouseUpEvent, _, _| {
                                    this.viewer_drag = None;
                                }),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                if this.viewer_dragged {
                                    this.viewer_dragged = false;
                                    return;
                                }
                                this.viewer = None;
                                cx.notify();
                            }))
                            .child(match image {
                                Some(image) => div()
                                    .id("viewer-image")
                                    .absolute()
                                    .left(px(img_left))
                                    .top(px(img_top))
                                    .on_click(cx.listener(|this, e: &gpui::ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        let dragged = this.viewer_dragged;
                                        this.viewer_dragged = false;
                                        this.viewer_drag = None;
                                        if dragged {
                                            return;
                                        }
                                        if e.click_count() >= 2 {
                                            this.viewer_toggle_actual(window);
                                            cx.notify();
                                        }
                                    }))
                                    .child(
                                        img(image)
                                            .w(px(dw))
                                            .h(px(dh))
                                            .object_fit(gpui::ObjectFit::Contain),
                                    )
                                    .into_any_element(),
                                None => div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(hsla(0., 0., 1., 0.75))
                                    .child("Đang tải ảnh… (ảnh đã hết hạn trên máy chủ sẽ không mở được)")
                                    .into_any_element(),
                            })
                            .when_some(caption, |el, text| {
                                el.child(
                                    div()
                                        .absolute()
                                        .bottom(px(8.))
                                        .left(px(16.))
                                        .right(px(16.))
                                        .text_size(px(13.))
                                        .text_color(hsla(0., 0., 1., 0.9))
                                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(text),
                                )
                            })
                            .when(has_prev, |el| {
                                el.child(
                                    arrow("viewer-prev-wrap", "‹", true).on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.viewer_step(-1, window, cx);
                                    })),
                                )
                            })
                            .when(has_next, |el| {
                                el.child(
                                    arrow("viewer-next-wrap", "›", false).on_click(cx.listener(
                                        |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.viewer_step(1, window, cx);
                                        },
                                    )),
                                )
                            }),
                    )
                    .when(ocr_busy || ocr_result.is_some(), |el| {
                        el.child(ocr_result_panel(ocr_busy, ocr_result.clone(), cx))
                    })
                    .child(
                        div()
                            .id("viewer-footer")
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .justify_center()
                            .gap(px(8.))
                            .px(px(12.))
                            .py(px(8.))
                            .bg(rgb(0x2e3031))
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .child(btn("viewer-out", "Thu nhỏ").on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                let z = this.viewer.as_ref().map(|v| v.zoom).unwrap_or(1.0);
                                this.set_zoom(z / 1.25);
                                cx.notify();
                            })))
                            .child(
                                div()
                                    .min_w(px(48.))
                                    .text_size(px(13.))
                                    .text_color(rgb(0xffffff))
                                    .child(format!("{percent:.0}%")),
                            )
                            .child(btn("viewer-in", "Phóng to").on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                let z = this.viewer.as_ref().map(|v| v.zoom).unwrap_or(1.0);
                                this.set_zoom(z * 1.25);
                                cx.notify();
                            })))
                            .child(btn("viewer-fit", "Vừa khung").on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.set_zoom(1.0);
                                cx.notify();
                            })))
                            .child(btn("viewer-actual", "100%").on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.viewer_set_actual(window);
                                cx.notify();
                            })))
                            .child(btn("viewer-rot-l", "Xoay trái").on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.viewer_rotate(-1, cx);
                            })))
                            .child(btn("viewer-rot-r", "Xoay phải").on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.viewer_rotate(1, cx);
                            })))
                            .child(btn("viewer-save", "Lưu về máy").on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.save_media(&save);
                                cx.notify();
                            })))
                            .child(btn("viewer-copy", "Copy").on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.copy_image(&copy, cx);
                                cx.notify();
                            })))
                            .when(can_ocr, |el| {
                                el.child(btn("viewer-ocr", if ocr_busy { "Đang quét…" } else { "Quét chữ" }).on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.run_viewer_ocr(ocr_msg.clone(), Some(window), cx);
                                        cx.notify();
                                    }),
                                ))
                            }),
                    ),
            )
            .into_any_element()
    }
}

/// Shell size inside the client-side shadow, in logical pixels.
fn viewer_box(window: &Window) -> (f32, f32) {
    let vp = window.viewport_size();
    let csd = if crate::window_frame::CsdCorners::from_window(window).any_pad() {
        zalo_design::WINDOW_SHADOW * 2.0
    } else {
        0.0
    };
    (
        (f32::from(vp.width) - csd).max(320.0),
        (f32::from(vp.height) - csd).max(240.0),
    )
}

/// Decoded pixels when the bitmap is ready, otherwise the declared size
/// with width and height swapped on an odd rotation.
fn viewer_pixels(v: &Viewer) -> (f32, f32) {
    if let Some(image) = &v.image {
        let s = image.size(0);
        let (w, h) = (s.width.0 as f32, s.height.0 as f32);
        if w >= 1.0 && h >= 1.0 {
            return (w, h);
        }
    }
    let mut w = v.msg.media_w.max(1) as f32;
    let mut h = v.msg.media_h.max(1) as f32;
    if v.turns % 2 == 1 {
        std::mem::swap(&mut w, &mut h);
    }
    (w, h)
}

pub(crate) fn rotate_turns(rgba: RgbaImage, turns: u8) -> RgbaImage {
    match turns % 4 {
        1 => image::imageops::rotate90(&rgba),
        2 => image::imageops::rotate180(&rgba),
        3 => image::imageops::rotate270(&rgba),
        _ => rgba,
    }
}

/// Status text used when the R2 OCR runtime & models have not been installed yet.
#[must_use]
pub(crate) fn ocr_missing_message() -> String {
    "Chưa cài thư viện OCR.".to_owned()
}

/// One text line placed on the image, in the same fractions the original
/// overlay uses (`quads` on the scanned bitmap).
#[derive(Clone, Debug)]
pub(crate) struct OcrBox {
    /// Recognized text.
    pub text: String,
    /// Left edge, 0..1 of the image width.
    pub x: f32,
    /// Top edge, 0..1 of the image height.
    pub y: f32,
    /// Width, 0..1.
    pub w: f32,
    /// Height, 0..1.
    pub h: f32,
}

/// What `zocr` returned: the joined lines, and a box for each line.
#[derive(Clone, Debug)]
pub(crate) struct OcrScan {
    /// Lines joined with newlines, the copy buffer.
    pub text: String,
    /// Boxes in reading order, top to bottom.
    pub boxes: Vec<OcrBox>,
}

/// Decode compressed image bytes, apply viewer rotation, and run `zocr`.
///
/// The original session scans the open photo, keeps each line's quad, and
/// draws those quads on the picture. Copy is a separate action.
pub(crate) fn recognize_viewer_bytes(bytes: &[u8], turns: u8) -> Result<OcrScan, String> {
    let rgba = crate::media::decode_rgba(bytes, 4096)
        .ok_or_else(|| "Không giải mã được ảnh để quét chữ.".to_owned())?;
    let rgb = image::DynamicImage::ImageRgba8(rotate_turns(rgba, turns)).into_rgb8();
    match zocr::recognize_lines(&rgb) {
        Ok(lines) => scan_from_lines(&lines, rgb.width(), rgb.height()),
        Err(zocr::OcrError::ModelMissing) => Err(ocr_missing_message()),
        Err(err) => Err(err.to_string()),
    }
}

fn scan_from_lines(lines: &[zocr::Line], width: u32, height: u32) -> Result<OcrScan, String> {
    let text = lines
        .iter()
        .map(|line| line.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return Err("Không tìm thấy văn bản trong ảnh.".to_owned());
    }
    let w = width.max(1) as f32;
    let h = height.max(1) as f32;
    let boxes = lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .map(|line| box_of(line, w, h))
        .collect();
    Ok(OcrScan { text, boxes })
}

fn box_of(line: &zocr::Line, width: f32, height: f32) -> OcrBox {
    let min_x = line.quad.iter().map(|p| p[0]).fold(f64::MAX, f64::min) as f32;
    let min_y = line.quad.iter().map(|p| p[1]).fold(f64::MAX, f64::min) as f32;
    let max_x = line.quad.iter().map(|p| p[0]).fold(0.0_f64, f64::max) as f32;
    let max_y = line.quad.iter().map(|p| p[1]).fold(0.0_f64, f64::max) as f32;
    OcrBox {
        text: line.text.clone(),
        x: (min_x / width).clamp(0.0, 1.0),
        y: (min_y / height).clamp(0.0, 1.0),
        w: ((max_x - min_x) / width).clamp(0.0, 1.0),
        h: ((max_y - min_y) / height).clamp(0.0, 1.0),
    }
}

/// Screen-pixel `(left, top, width, height)` of an OCR box within a `dw × dh` photo.
#[must_use]
pub(crate) fn ocr_box_rect(mark: &OcrBox, dw: f32, dh: f32) -> (f32, f32, f32, f32) {
    let pad_x = 2.0;
    let pad_y = 1.5;
    let left = (mark.x * dw - pad_x).clamp(0.0, dw.max(0.0));
    let top = (mark.y * dh - pad_y).clamp(0.0, dh.max(0.0));
    let w = (mark.w * dw + pad_x * 2.0).max(10.0).min((dw - left).max(0.0));
    let h = (mark.h * dh + pad_y * 2.0).max(12.0).min((dh - top).max(0.0));
    (left, top, w, h)
}

/// Index of the OCR box containing image-local point `(lx, ly)`.
#[must_use]
pub(crate) fn ocr_box_at(boxes: &[OcrBox], lx: f32, ly: f32, dw: f32, dh: f32) -> Option<usize> {
    // Prefer the smallest enclosing box if two boxes overlap slightly.
    boxes
        .iter()
        .enumerate()
        .filter_map(|(i, mark)| {
            let (left, top, w, h) = ocr_box_rect(mark, dw, dh);
            if lx >= left && lx <= left + w && ly >= top && ly <= top + h {
                Some((i, w * h))
            } else {
                None
            }
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// All OCR box indices intersecting the drag rectangle `(x0, y0) -> (x1, y1)`.
#[must_use]
pub(crate) fn ocr_boxes_in_rect(
    boxes: &[OcrBox],
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    dw: f32,
    dh: f32,
) -> std::collections::BTreeSet<usize> {
    let min_x = x0.min(x1);
    let max_x = x0.max(x1);
    let min_y = y0.min(y1);
    let max_y = y0.max(y1);
    let mut out = std::collections::BTreeSet::new();
    for (i, mark) in boxes.iter().enumerate() {
        let (left, top, w, h) = ocr_box_rect(mark, dw, dh);
        let overlaps = left <= max_x && left + w >= min_x && top <= max_y && top + h >= min_y;
        if overlaps {
            out.insert(i);
        }
    }
    out
}

/// Join selected OCR boxes in reading order, falling back to all boxes when `selected` is empty.
#[must_use]
pub(crate) fn ocr_selected_text(
    boxes: &[OcrBox],
    selected: &std::collections::BTreeSet<usize>,
) -> String {
    if selected.is_empty() {
        boxes
            .iter()
            .map(|b| b.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        selected
            .iter()
            .filter_map(|&i| boxes.get(i))
            .map(|b| b.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Electron-style Live-Text highlight layer drawn directly over the photo.
/// Lets the original photo pixels show through cleanly — unselected boxes use a
/// subtle translucent veil and selected boxes highlight in Zalo Blue (`#0068ff`).
pub(crate) fn ocr_marks(
    boxes: &[OcrBox],
    selected: &std::collections::BTreeSet<usize>,
    drag_rect: Option<(f32, f32, f32, f32)>,
    dw: f32,
    dh: f32,
) -> gpui::AnyElement {
    let marquee = drag_rect.and_then(|(x0, y0, x1, y1)| {
        let min_x = x0.min(x1).clamp(0.0, dw);
        let max_x = x0.max(x1).clamp(0.0, dw);
        let min_y = y0.min(y1).clamp(0.0, dh);
        let max_y = y0.max(y1).clamp(0.0, dh);
        let w = max_x - min_x;
        let h = max_y - min_y;
        (w > 3.0 || h > 3.0).then_some((min_x, min_y, w.max(2.0), h.max(2.0)))
    });
    div()
        .id("ocr-marks")
        .absolute()
        .top_0()
        .left_0()
        .w(px(dw))
        .h(px(dh))
        .cursor(CursorStyle::IBeam)
        .children(boxes.iter().enumerate().map(|(i, mark)| {
            let (left, top, w, h) = ocr_box_rect(mark, dw, dh);
            let is_sel = selected.contains(&i);
            div()
                .id(("ocr-box", i as u64))
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(w))
                .h(px(h))
                .rounded(px(3.))
                .cursor(CursorStyle::IBeam)
                .when(is_sel, |el| {
                    el.bg(hsla(0., 0., 1., 0.32))
                        .border_1()
                        .border_color(hsla(0., 0., 1., 0.78))
                })
                .when(!is_sel, |el| {
                    el.bg(hsla(0., 0., 1., 0.13))
                        .hover(|st| {
                            st.bg(hsla(0., 0., 1., 0.24))
                                .border_1()
                                .border_color(hsla(0., 0., 1., 0.36))
                        })
                })
        }))
        .when_some(marquee, |el, (x, y, w, h)| {
            el.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w))
                    .h(px(h))
                    .rounded(px(2.))
                    .bg(hsla(0., 0., 1., 0.16))
                    .border_1()
                    .border_color(hsla(0., 0., 1., 0.72)),
            )
        })
        .into_any_element()
}

fn ocr_result_panel(
    busy: bool,
    result: Option<Result<String, String>>,
    cx: &mut Context<ShellView>,
) -> impl IntoElement {
    let copy_text = result.as_ref().and_then(|r| r.as_ref().ok()).cloned();
    div()
        .id("viewer-ocr-panel")
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(16.))
        .py(px(10.))
        .bg(rgb(0x232526))
        .border_t_1()
        .border_color(hsla(0., 0., 1., 0.12))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(12.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(hsla(0., 0., 1., 0.8))
                        .child("Văn bản trong ảnh (OCR)"),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.))
                        .when_some(copy_text, |el, text| {
                            el.child(
                                div()
                                    .id("viewer-ocr-copy")
                                    .px(px(10.))
                                    .py(px(3.))
                                    .rounded(px(4.))
                                    .bg(hsla(0., 0., 1., 0.14))
                                    .hover(|s| s.bg(hsla(0., 0., 1., 0.24)))
                                    .cursor_pointer()
                                    .text_size(px(12.))
                                    .text_color(rgb(0xffffff))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        crate::clip::copy_text(cx, text.clone());
                                        this.set_note("Đã sao chép văn bản OCR");
                                        cx.notify();
                                    }))
                                    .child("Chép chữ"),
                            )
                        })
                        .child(
                            div()
                                .id("viewer-ocr-hide")
                                .px(px(8.))
                                .py(px(3.))
                                .rounded(px(4.))
                                .bg(hsla(0., 0., 1., 0.1))
                                .hover(|s| s.bg(hsla(0., 0., 1., 0.2)))
                                .cursor_pointer()
                                .text_size(px(12.))
                                .text_color(hsla(0., 0., 1., 0.85))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    if let Some(v) = this.viewer.as_mut() {
                                        v.ocr_result = None;
                                    }
                                    cx.notify();
                                }))
                                .child("Ẩn"),
                        ),
                ),
        )
        .child(
            div()
                .id("viewer-ocr-body")
                .max_h(px(120.))
                .overflow_y_scroll()
                .text_size(px(13.))
                .text_color(if matches!(result, Some(Err(_))) {
                    rgb(0xff_b4_b4)
                } else {
                    rgb(0xffffff)
                })
                .child(if busy {
                    "Đang nhận dạng văn bản…".to_owned()
                } else {
                    match result {
                        Some(Ok(text)) => text,
                        Some(Err(err)) => err,
                        None => String::new(),
                    }
                }),
        )
}

/// Keeps a zoomed photo from sliding completely off the stage. A photo that
/// still fits stays centred.
fn clamp_viewer_pan(
    pan_x: f32,
    pan_y: f32,
    dw: f32,
    dh: f32,
    stage_w: f32,
    stage_h: f32,
) -> (f32, f32) {
    let max_x = ((dw - stage_w) * 0.5).max(0.0);
    let max_y = ((dh - stage_h) * 0.5).max(0.0);
    (pan_x.clamp(-max_x, max_x), pan_y.clamp(-max_y, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_stays_inside_the_stage() {
        assert_eq!(
            clamp_viewer_pan(40.0, -20.0, 100.0, 80.0, 200.0, 200.0),
            (0.0, 0.0)
        );
        assert_eq!(
            clamp_viewer_pan(80.0, -90.0, 300.0, 200.0, 200.0, 100.0),
            (50.0, -50.0)
        );
    }

    #[test]
    fn scan_places_each_line_on_the_image() {
        let line = zocr::Line {
            text: "Xin chào".into(),
            score: 0.99,
            detection: 0.9,
            quad: [[10.0, 20.0], [110.0, 20.0], [110.0, 40.0], [10.0, 40.0]],
        };
        let scan = scan_from_lines(&[line], 200, 100).expect("scan");
        assert_eq!(scan.text, "Xin chào");
        let box_ = &scan.boxes[0];
        assert!((box_.x - 0.05).abs() < 0.001);
        assert!((box_.y - 0.2).abs() < 0.001);
        assert!((box_.w - 0.5).abs() < 0.001);
        assert!((box_.h - 0.2).abs() < 0.001);
    }

    #[test]
    fn ocr_missing_hint_and_bad_image_error() {
        assert!(ocr_missing_message().contains("OCR"));
        let err = recognize_viewer_bytes(b"not-an-image", 1).unwrap_err();
        assert!(err.contains("giải mã"));
    }

    #[test]
    fn ocr_live_text_hit_test_marquee_and_copy_join() {
        let boxes = vec![
            OcrBox {
                text: "Shopee Dũng Mobile".into(),
                x: 0.1,
                y: 0.1,
                w: 0.5,
                h: 0.1,
            },
            OcrBox {
                text: "Lịch làm việc".into(),
                x: 0.1,
                y: 0.3,
                w: 0.4,
                h: 0.1,
            },
            OcrBox {
                text: "My Documents".into(),
                x: 0.1,
                y: 0.6,
                w: 0.4,
                h: 0.1,
            },
        ];
        // Hit inside box 0 on a 400x200 displayed photo.
        assert_eq!(ocr_box_at(&boxes, 100.0, 30.0, 400.0, 200.0), Some(0));
        assert_eq!(ocr_box_at(&boxes, 350.0, 180.0, 400.0, 200.0), None);
        // Drag marquee across boxes 0 and 1 selects both, skipping box 2.
        let sel = ocr_boxes_in_rect(&boxes, 50.0, 25.0, 180.0, 75.0, 400.0, 200.0);
        assert_eq!(sel.into_iter().collect::<Vec<_>>(), vec![0, 1]);
        // Selected text joins only chosen lines; empty selection falls back to all lines.
        let only_first: std::collections::BTreeSet<usize> = [0].into_iter().collect();
        assert_eq!(ocr_selected_text(&boxes, &only_first), "Shopee Dũng Mobile");
        let empty = std::collections::BTreeSet::new();
        assert_eq!(
            ocr_selected_text(&boxes, &empty),
            "Shopee Dũng Mobile\nLịch làm việc\nMy Documents"
        );
    }
}
