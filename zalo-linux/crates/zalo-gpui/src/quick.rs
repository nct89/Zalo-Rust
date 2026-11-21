//! "Quick Messages" (quick replies): `/shortcut` bar above the composer,
//! lightning-button popup, and settings manager dialog persisted to `ui-state.json`.

use gpui::{
    div, prelude::*, px, rgb, ClickEvent, Context, Entity, Focusable, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement, Styled, Window,
};
use serde_json::{json, Value};

use crate::color::token_rgb;
use crate::dialogs::Dialog;
use crate::fonts::ic;
use crate::line_input::LineInput;
use crate::prefs::Prefs;
use crate::shell::ShellView;

/// Maximum number of saved quick replies (matching `zalo-gui`).
const MAX_QUICK: usize = 200;

/// One saved quick-reply template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickReply {
    /// Server-assigned quick-message id, when synced with Zalo cloud.
    pub id: Option<i64>,
    /// Shortcut trigger without leading `/`.
    pub key: String,
    /// Full message text inserted into the composer.
    pub text: String,
}

/// Modal state for "Manage Quick Messages".
pub struct QuickMessagesDlg {
    /// Shortcut input field.
    pub key_input: Entity<LineInput>,
    /// Body text input field.
    pub text_input: Entity<LineInput>,
    /// True when the add/edit form is expanded.
    pub form_open: bool,
    /// Server id of the quick reply currently being edited, if any.
    pub editing_id: Option<i64>,
    /// True while a server sync request is in flight.
    pub syncing: bool,
    /// Non-empty validation message.
    pub error: Option<String>,
}

/// Normalize a user-entered shortcut key (strips `/` and whitespace).
#[must_use]
pub(crate) fn normalize_key(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('/')
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Parse saved quick-reply templates from [`Prefs`].
#[must_use]
pub(crate) fn load_quick(prefs: &Prefs) -> Vec<QuickReply> {
    let raw = prefs.raw("quick");
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    items
        .into_iter()
        .take(MAX_QUICK)
        .filter_map(|item| {
            let key = normalize_key(item.get("key")?.as_str()?);
            let text = item.get("text")?.as_str()?.trim().to_owned();
            if key.is_empty() || text.is_empty() {
                return None;
            }
            let id = item.get("id").and_then(Value::as_i64);
            Some(QuickReply { id, key, text })
        })
        .collect()
}

/// Persist the full list into [`Prefs`] (`"quick"` in `ui-state.json`).
pub(crate) fn save_quick(prefs: &mut Prefs, list: &[QuickReply]) {
    let arr: Vec<Value> = list
        .iter()
        .take(MAX_QUICK)
        .map(|q| match q.id {
            Some(id) => json!({ "id": id, "key": q.key, "text": q.text }),
            None => json!({ "key": q.key, "text": q.text }),
        })
        .collect();
    prefs.set("quick", Value::Array(arr));
}

/// Merges cloud quick messages with unsynced local-only entries.
pub(crate) fn merge_server_quick(
    local: &[QuickReply],
    remote: &[zalo_client::features::QuickMessageItem],
) -> Vec<QuickReply> {
    let mut out: Vec<QuickReply> = remote
        .iter()
        .filter_map(|item| {
            let key = normalize_key(&item.keyword);
            let text = item.text.trim().to_owned();
            if key.is_empty() || text.is_empty() {
                return None;
            }
            Some(QuickReply {
                id: Some(item.id),
                key,
                text,
            })
        })
        .collect();
    for item in local {
        if item.id.is_none() && !out.iter().any(|r| r.key.eq_ignore_ascii_case(&item.key)) {
            out.push(item.clone());
        }
    }
    out.truncate(MAX_QUICK);
    out
}

/// True when quick-reply suggestions are enabled (`quickOff` is false/unset).
#[must_use]
pub(crate) fn quick_enabled(prefs: &Prefs) -> bool {
    !prefs.bool("quickOff", false)
}

/// Templates matching a leading `/shortcut` draft (prefix matches first, up to 8).
#[must_use]
pub(crate) fn quick_matches<'a>(items: &'a [QuickReply], draft: &str) -> Vec<&'a QuickReply> {
    let Some(tail) = draft.trim_start().strip_prefix('/') else {
        return Vec::new();
    };
    if tail.contains(char::is_whitespace) {
        return Vec::new();
    }
    let q = tail.to_lowercase();
    let mut prefix = Vec::new();
    let mut contains = Vec::new();
    for item in items {
        let lower = item.key.to_lowercase();
        if q.is_empty() || lower.starts_with(&q) {
            prefix.push(item);
        } else if lower.contains(&q) {
            contains.push(item);
        }
    }
    prefix.into_iter().chain(contains).take(8).collect()
}

impl ShellView {
    /// Open "Manage Quick Messages" modal dialog.
    pub(crate) fn open_quick_messages(&mut self, cx: &mut Context<Self>) {
        self.quick_menu = false;
        self.rail_popup = None;
        let key_input = cx.new(|cx| LineInput::new(cx, "Phím tắt (vd: xinchao)"));
        let text_input = cx.new(|cx| LineInput::new(cx, "Nội dung tin nhắn"));
        self.dialog = Some(Dialog::QuickMessages(QuickMessagesDlg {
            key_input,
            text_input,
            form_open: false,
            editing_id: None,
            syncing: false,
            error: None,
        }));
        self.sync_quick_messages(None, None, cx);
    }

    /// Background sync with `/api/quickmessage/{list,create,update,delete}`.
    fn sync_quick_messages(
        &mut self,
        upsert: Option<(Option<i64>, String, String)>,
        delete_id: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        if let Some(Dialog::QuickMessages(dlg)) = self.dialog.as_mut() {
            dlg.syncing = true;
        }
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        if let Some(id) = delete_id {
                            api.delete_quick_messages(&[id])
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                        if let Some((item_id, key, text)) = upsert {
                            match item_id {
                                Some(id) => {
                                    api.update_quick_message(id, &key, &text)
                                        .await
                                        .map_err(|e| e.to_string())?;
                                }
                                None => {
                                    api.create_quick_message(&key, &text)
                                        .await
                                        .map_err(|e| e.to_string())?;
                                }
                            }
                        }
                        api.list_quick_messages(0, 0)
                            .await
                            .map(|v| zalo_client::features::quick_messages_from(&v).items)
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Ok(remote) = got {
                    let local = load_quick(&view.prefs);
                    let merged = merge_server_quick(&local, &remote);
                    save_quick(&mut view.prefs, &merged);
                }
                if let Some(Dialog::QuickMessages(dlg)) = view.dialog.as_mut() {
                    dlg.syncing = false;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn insert_quick_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_menu = false;
        let composer = self.composer.clone();
        composer.update(cx, |input, cx| {
            input.set_text(text.clone(), cx);
        });
        composer.read(cx).focus_handle(cx).focus(window);
        self.on_composer_media_change(&text, cx);
    }

    /// Floating suggestion bar above the composer when typing `/shortcut`.
    pub(crate) fn quick_bar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !quick_enabled(&self.prefs) || self.quick_menu {
            return None;
        }
        let draft = self.composer.read(cx).text();
        let all = load_quick(&self.prefs);
        let matches: Vec<QuickReply> = quick_matches(&all, &draft).into_iter().cloned().collect();
        if matches.is_empty() {
            return None;
        }
        let bg = token_rgb("--layer-background", "#ffffff");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let blue = token_rgb("--B55", "#0068ff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        Some(
            div()
                .id("quick-bar")
                .flex()
                .flex_col()
                .w_full()
                .px(px(12.))
                .py(px(4.))
                .bg(rgb(bg))
                .border_t_1()
                .border_color(crate::color::border_subtle())
                .children(matches.into_iter().enumerate().map(|(i, item)| {
                    let body = item.text.clone();
                    div()
                        .id(("quick-bar-item", i as u64))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(34.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(hover)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.insert_quick_text(body.clone(), window, cx);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(blue))
                                .child(crate::fonts::icon_el(ic::TB_QUICK, "⚡", 14.)),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(blue))
                                .child(format!("/{}", item.key)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_size(px(13.))
                                .text_color(rgb(ink))
                                .truncate()
                                .child(item.text),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(muted))
                                .child("Chèn"),
                        )
                }))
                .into_any_element(),
        )
    }

    /// Popover anchored above the composer lightning (`cmp-qr`) button.
    pub(crate) fn quick_popup(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.quick_menu {
            return None;
        }
        let items = load_quick(&self.prefs);
        let off = !quick_enabled(&self.prefs);
        let bg = token_rgb("--layer-background", "#ffffff");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let blue = token_rgb("--B55", "#0068ff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        Some(
            div()
                .id("quick-popup")
                .flex()
                .flex_col()
                .w_full()
                .max_h(px(280.))
                .bg(rgb(bg))
                .border_t_1()
                .border_color(crate::color::border_subtle())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px(px(16.))
                        .py(px(8.))
                        .border_b_1()
                        .border_color(crate::color::border_vanish())
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_family(crate::fonts::FAMILY_SEMIBOLD)
                                .text_color(rgb(ink))
                                .child("Tin nhắn nhanh"),
                        )
                        .child(
                            div()
                                .id("quick-popup-manage")
                                .text_size(px(13.))
                                .text_color(rgb(blue))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.open_quick_messages(cx);
                                    cx.notify();
                                }))
                                .child("Quản lý"),
                        ),
                )
                .child(if off {
                    div()
                        .px(px(16.))
                        .py(px(12.))
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .child("Gợi ý /phím tắt đang tắt. Bấm «Quản lý» để bật lại hoặc sửa mẫu.")
                        .into_any_element()
                } else if items.is_empty() {
                    div()
                        .px(px(16.))
                        .py(px(12.))
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .child("Chưa có tin nhắn nhanh. Bấm «Quản lý» để tạo phím tắt / mẫu.")
                        .into_any_element()
                } else {
                    div()
                        .id("quick-popup-list")
                        .flex()
                        .flex_col()
                        .max_h(px(210.))
                        .overflow_y_scroll()
                        .px(px(8.))
                        .py(px(4.))
                        .children(items.into_iter().take(20).enumerate().map(|(i, item)| {
                            let body = item.text.clone();
                            div()
                                .id(("quick-pop-row", i as u64))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(10.))
                                .px(px(10.))
                                .py(px(7.))
                                .rounded(px(6.))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(hover)))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.insert_quick_text(body.clone(), window, cx);
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .min_w(px(80.))
                                        .text_size(px(13.))
                                        .text_color(rgb(blue))
                                        .child(format!("/{}", item.key)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .text_size(px(13.))
                                        .text_color(rgb(ink))
                                        .truncate()
                                        .child(item.text),
                                )
                        }))
                        .into_any_element()
                })
                .into_any_element(),
        )
    }

    /// Render the "Manage Quick Messages" modal inside [`ShellView::dialog_overlay`].
    pub(crate) fn quick_messages_el(
        &self,
        d: &QuickMessagesDlg,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let red = token_rgb("--R60", "#c31818");
        let chip_bg = token_rgb("--N20", "#eaedf0");
        let line = crate::color::border_subtle();
        let items = load_quick(&self.prefs);
        let enabled = quick_enabled(&self.prefs);

        let body = if d.form_open {
            let field = |label: &'static str, input: Entity<LineInput>| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().text_size(px(13.)).text_color(rgb(muted)).child(label))
                    .child(
                        div()
                            .h(px(38.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .text_size(px(14.))
                            .child(div().flex_1().child(input)),
                    )
            };
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .p(px(16.))
                .child(field("Phím tắt (không cần dấu /)", d.key_input.clone()))
                .child(field("Nội dung tin nhắn", d.text_input.clone()))
                .into_any_element()
        } else {
            div()
                .id("qm-list")
                .flex()
                .flex_col()
                .max_h(px(400.))
                .overflow_y_scroll()
                .child(
                    div()
                        .id("qm-toggle-row")
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px(px(16.))
                        .py(px(12.))
                        .border_b_1()
                        .border_color(line)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let next = !quick_enabled(&this.prefs);
                            this.prefs.set("quickOff", Value::Bool(!next));
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_size(px(14.))
                                .child("Hiện gợi ý khi gõ /phím tắt trong ô soạn tin"),
                        )
                        .child(crate::settings::switch_el(enabled)),
                )
                .when(items.is_empty(), |el| {
                    el.child(
                        div()
                            .p(px(16.))
                            .text_size(px(14.))
                            .text_color(rgb(muted))
                            .child("Chưa có tin nhắn nhanh. Bấm «Tạo mới» để thêm phím tắt."),
                    )
                })
                .children(items.into_iter().enumerate().map(|(i, item)| {
                    let edit_item = item.clone();
                    let del_key = item.key.clone();
                    let del_id = item.id;
                    div()
                        .id(("qm-row", i as u64))
                        .flex()
                        .flex_none()
                        .flex_row()
                        .items_center()
                        .gap(px(12.))
                        .px(px(16.))
                        .py(px(10.))
                        .border_b_1()
                        .border_color(line)
                        .child(
                            div()
                                .px(px(8.))
                                .py(px(3.))
                                .rounded(px(4.))
                                .bg(rgb(chip_bg))
                                .text_size(px(13.))
                                .text_color(rgb(blue))
                                .child(format!("/{}", item.key)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_size(px(14.))
                                .truncate()
                                .child(item.text),
                        )
                        .child(
                            div()
                                .id(("qm-edit", i as u64))
                                .text_size(px(13.))
                                .text_color(rgb(blue))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    if let Some(Dialog::QuickMessages(dlg)) = this.dialog.as_mut() {
                                        let key_e = dlg.key_input.clone();
                                        let text_e = dlg.text_input.clone();
                                        dlg.form_open = true;
                                        dlg.editing_id = edit_item.id;
                                        dlg.error = None;
                                        key_e.update(cx, |inp, cx| {
                                            inp.set_text(edit_item.key.clone(), cx)
                                        });
                                        text_e.update(cx, |inp, cx| {
                                            inp.set_text(edit_item.text.clone(), cx)
                                        });
                                    }
                                    cx.notify();
                                }))
                                .child("Sửa"),
                        )
                        .child(
                            div()
                                .id(("qm-del", i as u64))
                                .text_size(px(13.))
                                .text_color(rgb(red))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let mut list = load_quick(&this.prefs);
                                    list.retain(|q| q.key != del_key);
                                    save_quick(&mut this.prefs, &list);
                                    if let Some(id) = del_id {
                                        this.sync_quick_messages(None, Some(id), cx);
                                    }
                                    cx.notify();
                                }))
                                .child("Xóa"),
                        )
                }))
                .into_any_element()
        };

        div()
            .w(px(500.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_title_bar(
                if d.form_open {
                    "Mẫu tin nhắn nhanh"
                } else {
                    "Quản lý tin nhắn nhanh"
                },
                cx,
            ))
            .child(body)
            .when_some(d.error.clone(), |el, e| {
                el.child(
                    div()
                        .px(px(16.))
                        .text_size(px(13.))
                        .text_color(rgb(red))
                        .child(e),
                )
            })
            .child(if d.form_open {
                crate::dialogs::dialog_footer(
                    "Quay lại",
                    "Lưu",
                    true,
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(Dialog::QuickMessages(dlg)) = this.dialog.as_mut() {
                            dlg.form_open = false;
                            dlg.error = None;
                        }
                        cx.notify();
                    }),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.save_quick_form(cx);
                        cx.notify();
                    }),
                )
                .into_any_element()
            } else {
                crate::dialogs::dialog_footer(
                    "Đóng",
                    "Tạo mới",
                    true,
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dialog = None;
                        cx.notify();
                    }),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(Dialog::QuickMessages(dlg)) = this.dialog.as_mut() {
                            let key_e = dlg.key_input.clone();
                            let text_e = dlg.text_input.clone();
                            dlg.form_open = true;
                            dlg.editing_id = None;
                            dlg.error = None;
                            key_e.update(cx, |inp, cx| inp.clear(cx));
                            text_e.update(cx, |inp, cx| inp.clear(cx));
                        }
                        cx.notify();
                    }),
                )
                .into_any_element()
            })
    }

    fn save_quick_form(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::QuickMessages(dlg)) = self.dialog.as_mut() else {
            return;
        };
        let key = normalize_key(&dlg.key_input.read(cx).text());
        let text = dlg.text_input.read(cx).text().trim().to_owned();
        if key.is_empty() || text.is_empty() {
            dlg.error = Some("Nhập cả phím tắt và nội dung tin nhắn.".into());
            return;
        }
        let editing_id = dlg.editing_id.take();
        dlg.form_open = false;
        dlg.error = None;
        let mut list = load_quick(&self.prefs);
        let existing_id = editing_id.or_else(|| {
            list.iter()
                .find(|q| q.key == key)
                .and_then(|q| q.id)
        });
        list.retain(|q| q.key != key && (existing_id.is_none() || q.id != existing_id));
        list.push(QuickReply {
            id: existing_id,
            key: key.clone(),
            text: text.clone(),
        });
        save_quick(&mut self.prefs, &list);
        self.sync_quick_messages(Some((existing_id, key, text)), None, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_round_trip_and_filtering() {
        let mut prefs = Prefs::load(None);
        assert!(quick_enabled(&prefs));
        save_quick(
            &mut prefs,
            &[
                QuickReply {
                    id: Some(10),
                    key: normalize_key("/xinchao "),
                    text: "Xin chào anh/chị".into(),
                },
                QuickReply {
                    id: None,
                    key: "baogia".into(),
                    text: "Gửi bảng giá".into(),
                },
            ],
        );
        let loaded = load_quick(&prefs);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, Some(10));
        assert_eq!(loaded[0].key, "xinchao");
        assert_eq!(loaded[1].id, None);

        let merged = merge_server_quick(
            &loaded,
            &[zalo_client::features::QuickMessageItem {
                id: 10,
                keyword: "xinchao".into(),
                text: "Xin chào (cloud)".into(),
                item_type: 0,
            }],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "Xin chào (cloud)");
        assert_eq!(merged[1].key, "baogia");

        assert_eq!(quick_matches(&loaded, "/").len(), 2);
        assert_eq!(quick_matches(&loaded, "/xin").len(), 1);
        assert!(quick_matches(&loaded, "/xin thêm chữ").is_empty());
        assert!(quick_matches(&loaded, "xin").is_empty());

        prefs.set("quickOff", Value::Bool(true));
        assert!(!quick_enabled(&prefs));
    }
}
