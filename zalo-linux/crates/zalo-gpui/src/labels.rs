//! "Labels" written back to the account (`/api/convlabel/update`).
//!
//! The live `labelData` items carry more than `ConversationLabel` keeps
//! (`textKey`, `offset`, `emoji`, `createTime`), so every change is a
//! read-modify-write on the raw JSON: fetch the current list and version,
//! edit only what the user touched, send it back with that version.
//! A conversation sits in at most one label, as on Zalo PC.

use serde_json::{json, Value};

/// Zalo PC label colours (the six defaults first).
pub const COLORS: [&str; 8] = [
    "#d91b1b", "#f31bc8", "#ff6905", "#fac000", "#4bc377", "#0068ff", "#00a6a6", "#8c5ac8",
];

/// Label key of a thread id (`g_123` → `g123`, users unchanged).
#[must_use]
pub fn conv_key(thread_id: &str) -> String {
    match thread_id.strip_prefix("g_") {
        Some(rest) => format!("g{rest}"),
        None => thread_id.to_owned(),
    }
}

fn id_of(item: &Value) -> Option<i64> {
    item.get("id").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

/// Put `key` in label `target` (or in none), removing it everywhere else.
pub fn assign(items: &mut [Value], key: &str, target: Option<i64>) {
    for item in items.iter_mut() {
        let here = id_of(item) == target && target.is_some();
        let Some(list) = item.get_mut("conversations").and_then(Value::as_array_mut) else {
            if here {
                item["conversations"] = json!([key]);
            }
            continue;
        };
        list.retain(|c| c.as_str() != Some(key));
        if here {
            list.push(Value::String(key.to_owned()));
        }
    }
}

/// New label after the last one (id / offset + 1), same field set as Zalo.
pub fn add(items: &mut Vec<Value>, text: &str, color: &str, now_ms: i64) -> i64 {
    let id = items.iter().filter_map(id_of).max().unwrap_or(0) + 1;
    let offset = items
        .iter()
        .filter_map(|i| i.get("offset").and_then(Value::as_i64))
        .max()
        .unwrap_or(0)
        + 1;
    items.push(json!({
        "id": id, "text": text, "textKey": "", "conversations": [],
        "color": color, "offset": offset, "emoji": "", "createTime": now_ms,
    }));
    id
}

/// Rename / recolour one label, other fields untouched.
pub fn edit(items: &mut [Value], id: i64, text: &str, color: &str) {
    for item in items.iter_mut().filter(|i| id_of(i) == Some(id)) {
        item["text"] = Value::String(text.to_owned());
        item["color"] = Value::String(color.to_owned());
        // A renamed default no longer follows its translation key.
        if item.get("textKey").is_some() {
            item["textKey"] = Value::String(String::new());
        }
    }
}

/// Drop one label.
pub fn remove(items: &mut Vec<Value>, id: i64) {
    items.retain(|i| id_of(i) != Some(id));
}

/// `(items, version)` from a decrypted `convlabel/get` reply.
#[must_use]
pub fn raw_items(value: &Value) -> (Vec<Value>, i64) {
    let version = zalo_client::ConversationLabel::version_of(value);
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let items = match root.get("labelData") {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).ok(),
        Some(other) => Some(other.clone()),
        None => None,
    }
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();
    (items, version)
}

/// Fetch, apply `change`, write back, re-read. Blocking: run off the UI thread.
///
/// # Errors
///
/// Session, transport or Zalo errors as text.
pub fn update<F>(
    dir: &std::path::Path,
    change: F,
) -> Result<Vec<zalo_client::ConversationLabel>, String>
where
    F: FnOnce(&mut Vec<Value>) + Send + 'static,
{
    crate::bridge::block_api(dir, |api| async move {
        let current = api.get_labels().await.map_err(|e| e.to_string())?;
        let (mut items, version) = raw_items(&current);
        if items.is_empty() {
            return Err(
                "chưa đọc được danh sách thẻ hiện tại — không ghi để tránh mất dữ liệu".into(),
            );
        }
        change(&mut items);
        let data = serde_json::to_string(&items).map_err(|e| e.to_string())?;
        api.update_conversation_label(&data, version)
            .await
            .map_err(|e| e.to_string())?;
        let after = api.get_labels().await.map_err(|e| e.to_string())?;
        Ok(zalo_client::ConversationLabel::list_from(&after))
    })
}

/// Server-synced label mutation using Zalo 26.11.12 `/api/convlabel/manage/*` endpoints
/// with automatic fallback to legacy `/api/convlabel/update`.
#[derive(Clone, Debug)]
pub enum ManageOp {
    Add {
        text: String,
        color: String,
        now_ms: i64,
    },
    Update {
        id: i64,
        text: String,
        color: String,
        offset: i64,
    },
    Delete {
        id: i64,
    },
    Reorder {
        /// Ordered `(id, new_offset)` pairs.
        order: Vec<(i64, i64)>,
    },
}

/// Applies one [`ManageOp`] via `/api/convlabel/manage/*` (falling back to legacy
/// `/api/convlabel/update` if the server rejects the v2 path) and returns the refreshed list.
pub fn run_manage(
    dir: &std::path::Path,
    op: ManageOp,
) -> Result<Vec<zalo_client::ConversationLabel>, String> {
    crate::bridge::block_api(dir, |api| async move {
        let v2_ok = match &op {
            ManageOp::Add { text, color, .. } => {
                api.create_label_v2(text, color, "", &[]).await.is_ok()
            }
            ManageOp::Update {
                id,
                text,
                color,
                offset,
            } => api
                .update_label_v2(*id, text, color, *offset, &[], &[])
                .await
                .is_ok(),
            ManageOp::Delete { id } => api.delete_labels_v2(&[*id]).await.is_ok(),
            ManageOp::Reorder { order } => api.reorder_labels_v2(order).await.is_ok(),
        };
        if !v2_ok {
            let current = api.get_labels().await.map_err(|e| e.to_string())?;
            let (mut items, version) = raw_items(&current);
            if items.is_empty() {
                return Err(
                    "chưa đọc được danh sách thẻ hiện tại — không ghi để tránh mất dữ liệu".into(),
                );
            }
            match op {
                ManageOp::Add {
                    text,
                    color,
                    now_ms,
                } => {
                    add(&mut items, &text, &color, now_ms);
                }
                ManageOp::Update {
                    id, text, color, ..
                } => {
                    edit(&mut items, id, &text, &color);
                }
                ManageOp::Delete { id } => {
                    remove(&mut items, id);
                }
                ManageOp::Reorder { order } => {
                    for (id, off) in order {
                        if let Some(it) = items.iter_mut().find(|v| id_of(v) == Some(id)) {
                            it["offset"] = Value::from(off);
                        }
                    }
                    items.sort_by_key(|v| v.get("offset").and_then(Value::as_i64).unwrap_or(0));
                }
            }
            let data = serde_json::to_string(&items).map_err(|e| e.to_string())?;
            api.update_conversation_label(&data, version)
                .await
                .map_err(|e| e.to_string())?;
        }
        let after = api.get_labels().await.map_err(|e| e.to_string())?;
        Ok(zalo_client::ConversationLabel::list_from(&after))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> Value {
        json!({"data": {"labelData": "[{\"id\":1,\"text\":\"Khách hàng\",\"textKey\":\"\",\"conversations\":[\"g9\"],\"color\":\"#d91b1b\",\"offset\":1,\"emoji\":\"\",\"createTime\":5},{\"id\":2,\"text\":\"Gia đình\",\"textKey\":\"\",\"conversations\":[],\"color\":\"#f31bc8\",\"offset\":2,\"emoji\":\"\",\"createTime\":6}]", "version": 3}})
    }

    #[test]
    fn edits_keep_unknown_fields() {
        let (mut items, version) = raw_items(&live());
        assert_eq!((items.len(), version), (2, 3));
        assign(&mut items, "g9", Some(2));
        assert_eq!(items[0]["conversations"], json!([]));
        assert_eq!(items[1]["conversations"], json!(["g9"]));
        assert_eq!(items[0]["createTime"], 5, "untouched fields stay");
        assign(&mut items, "g9", None);
        assert_eq!(items[1]["conversations"], json!([]));
        let id = add(&mut items, "Mới", "#0068ff", 7);
        assert_eq!((id, items[2]["offset"].as_i64()), (3, Some(3)));
        edit(&mut items, 1, "VIP", "#4bc377");
        assert_eq!(
            (items[0]["text"].as_str(), items[0]["emoji"].as_str()),
            (Some("VIP"), Some(""))
        );
        remove(&mut items, 2);
        assert_eq!(items.len(), 2);
        assert_eq!(conv_key("g_12"), "g12");
    }
}

use gpui::{
    div, hsla, prelude::*, px, rgb, ClickEvent, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};

use crate::color::token_rgb;
use crate::dialogs::Dialog;
use crate::line_input::LineInput;
use crate::shell::ShellView;

/// "Manage Labels" dialog.
pub struct LabelsDlg {
    /// Editing `Some(id)` or creating `None`, with name field and colour.
    form: Option<(Option<i64>, Entity<LineInput>, String)>,
    /// Label waiting for a second "Delete" click.
    delete_armed: Option<i64>,
    busy: bool,
    error: Option<String>,
}

impl ShellView {
    /// Run one label change on the account, then refresh `self.labels`.
    fn apply_label_change<F>(&mut self, done: &'static str, change: F, cx: &mut Context<Self>)
    where
        F: FnOnce(&mut Vec<Value>) + Send + 'static,
    {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            self.set_note("Chưa đăng nhập — không ghi được thẻ phân loại");
            return;
        };
        if let Some(Dialog::Labels(d)) = self.dialog.as_mut() {
            d.busy = true;
        }
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move { update(&dir, change) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match got {
                    Ok(list) => {
                        view.labels = list;
                        view.conv_labels.clear();
                        if let Some(Dialog::Labels(d)) = view.dialog.as_mut() {
                            d.busy = false;
                            d.form = None;
                            d.delete_armed = None;
                            d.error = None;
                        }
                        view.set_note(done);
                    }
                    Err(e) => {
                        view.conv_labels.clear();
                        if let Some(Dialog::Labels(d)) = view.dialog.as_mut() {
                            d.busy = false;
                            d.error = Some(e.clone());
                        }
                        view.set_note(format!("Không ghi được thẻ phân loại: {e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Menu "Labels › <tag>": toggle that label on the conversation.
    pub(crate) fn toggle_conv_label(&mut self, conv: u64, label: i64, cx: &mut Context<Self>) {
        let Some(row) = self.conv(conv).cloned() else {
            return;
        };
        let target = if self.conv_label(conv) == Some(label) {
            None
        } else {
            Some(label)
        };
        // Optimistic: show it now, the reply replaces it.
        match target {
            Some(l) => self.conv_labels.insert(conv, l),
            None => self.conv_labels.remove(&conv),
        };
        let key = conv_key(&row.thread_id);
        self.apply_label_change(
            if target.is_some() {
                "Đã gắn thẻ phân loại"
            } else {
                "Đã bỏ thẻ phân loại"
            },
            move |items| assign(items, &key, target),
            cx,
        );
    }

    /// Menu "Manage Labels".
    pub(crate) fn open_labels(&mut self, cx: &mut Context<Self>) {
        let _ = cx;
        self.ensure_labels();
        self.menu = None;
        self.dialog = Some(Dialog::Labels(LabelsDlg {
            form: None,
            delete_armed: None,
            busy: false,
            error: None,
        }));
    }

    fn edit_label_form(&mut self, id: Option<i64>, cx: &mut Context<Self>) {
        let (name, color) = id
            .and_then(|id| self.labels.iter().find(|l| l.id == id))
            .map(|l| (l.text.clone(), l.color.clone()))
            .unwrap_or_else(|| (String::new(), COLORS[5].to_owned()));
        let input = cx.new(|cx| {
            let mut i = LineInput::new(cx, "Tên thẻ phân loại");
            i.set_text(name, cx);
            i
        });
        if let Some(Dialog::Labels(d)) = self.dialog.as_mut() {
            d.form = Some((id, input, color));
            d.error = None;
        }
    }

    fn apply_label_manage(&mut self, done: &'static str, op: ManageOp, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            self.set_note("Chưa đăng nhập — không ghi được thẻ phân loại");
            return;
        };
        if let Some(Dialog::Labels(d)) = self.dialog.as_mut() {
            d.busy = true;
        }
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move { run_manage(&dir, op) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match got {
                    Ok(list) => {
                        view.labels = list;
                        view.conv_labels.clear();
                        if let Some(Dialog::Labels(d)) = view.dialog.as_mut() {
                            d.busy = false;
                            d.form = None;
                            d.delete_armed = None;
                            d.error = None;
                        }
                        view.set_note(done);
                    }
                    Err(e) => {
                        view.conv_labels.clear();
                        if let Some(Dialog::Labels(d)) = view.dialog.as_mut() {
                            d.busy = false;
                            d.error = Some(e.clone());
                        }
                        view.set_note(format!("Không ghi được thẻ phân loại: {e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reorder_label_step(&mut self, idx: usize, delta: isize, cx: &mut Context<Self>) {
        let target = idx as isize + delta;
        if target < 0 || (target as usize) >= self.labels.len() {
            return;
        }
        let target = target as usize;
        self.labels.swap(idx, target);
        let order: Vec<(i64, i64)> = self
            .labels
            .iter()
            .enumerate()
            .map(|(i, l)| (l.id, (i + 1) as i64))
            .collect();
        self.apply_label_manage("Đã sắp xếp lại thẻ phân loại", ManageOp::Reorder { order }, cx);
    }

    fn save_label_form(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Labels(LabelsDlg {
            form: Some((id, input, color)),
            busy: false,
            ..
        })) = self.dialog.as_ref()
        else {
            return;
        };
        let name = input.read(cx).text().trim().to_owned();
        let (id, color) = (*id, color.clone());
        if name.is_empty() {
            if let Some(Dialog::Labels(d)) = self.dialog.as_mut() {
                d.error = Some("Nhập tên thẻ".into());
            }
            return;
        }
        let now = crate::data::now_ms();
        match id {
            Some(id) => {
                let offset = self
                    .labels
                    .iter()
                    .position(|l| l.id == id)
                    .map(|i| (i + 1) as i64)
                    .unwrap_or(1);
                self.apply_label_manage(
                    "Đã sửa thẻ phân loại",
                    ManageOp::Update {
                        id,
                        text: name,
                        color,
                        offset,
                    },
                    cx,
                );
            }
            None => self.apply_label_manage(
                "Đã tạo thẻ phân loại",
                ManageOp::Add {
                    text: name,
                    color,
                    now_ms: now,
                },
                cx,
            ),
        }
    }

    pub(crate) fn labels_el(&self, d: &LabelsDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let red = token_rgb("--R60", "#c31818");
        let body = match &d.form {
            Some((_, input, picked)) => div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .p(px(16.))
                .child(
                    div()
                        .h(px(40.))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(crate::color::border_subtle())
                        .text_size(px(15.))
                        .child(div().flex_1().child(input.clone())),
                )
                .child(div().text_size(px(13.)).text_color(rgb(muted)).child("Màu thẻ"))
                .child(div().flex().flex_row().gap(px(10.)).children(COLORS.iter().map(|c| {
                    let on = picked.eq_ignore_ascii_case(c);
                    let value = (*c).to_owned();
                    div()
                        .id(gpui::SharedString::from(format!("label-color-{c}")))
                        .size(px(28.))
                        .rounded(px(14.))
                        .border_2()
                        .border_color(if on { rgb(ink).into() } else { hsla(0., 0., 0., 0.) })
                        .p(px(3.))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(Dialog::Labels(LabelsDlg { form: Some((_, _, color)), .. })) = this.dialog.as_mut() {
                                *color = value.clone();
                            }
                            cx.notify();
                        }))
                        .child(div().size_full().rounded(px(11.)).bg(rgb(crate::color::parse_hex(c).unwrap_or(0x0068ff))))
                })))
                .into_any_element(),
            None => div()
                .id("labels-list")
                .flex()
                .flex_col()
                .max_h(px(440.))
                .overflow_y_scroll()
                .when(self.labels.is_empty(), |el| {
                    el.child(div().p(px(16.)).text_size(px(14.)).text_color(rgb(muted)).child(self.labels_status()))
                })
                .children(self.labels.iter().enumerate().map(|(idx, l)| {
                    let (eid, did) = (l.id, l.id);
                    let armed = d.delete_armed == Some(l.id);
                    let can_up = idx > 0 && !d.busy;
                    let can_down = idx + 1 < self.labels.len() && !d.busy;
                    div()
                        .id(("label-row", l.id as u64))
                        .flex()
                        .flex_none()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(48.))
                        .px(px(16.))
                        .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
                        .child(div().size(px(14.)).rounded(px(3.)).bg(rgb(crate::color::parse_hex(&l.color).unwrap_or(0x0068ff))))
                        .child(div().flex_1().min_w(px(0.)).text_size(px(15.)).truncate().child(l.text.clone()))
                        .child(div().text_size(px(12.)).text_color(rgb(muted)).child(format!("{} hội thoại", l.conversations.len())))
                        .child(
                            div()
                                .id(("label-up", l.id as u64))
                                .px(px(4.))
                                .text_size(px(12.))
                                .text_color(rgb(if can_up { ink } else { muted }))
                                .when(can_up, |el| {
                                    el.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.reorder_label_step(idx, -1, cx);
                                        cx.notify();
                                    }))
                                })
                                .child("▲"),
                        )
                        .child(
                            div()
                                .id(("label-down", l.id as u64))
                                .px(px(4.))
                                .text_size(px(12.))
                                .text_color(rgb(if can_down { ink } else { muted }))
                                .when(can_down, |el| {
                                    el.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.reorder_label_step(idx, 1, cx);
                                        cx.notify();
                                    }))
                                })
                                .child("▼"),
                        )
                        .child(
                            div()
                                .id(("label-edit", l.id as u64))
                                .text_size(px(13.))
                                .text_color(rgb(blue))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.edit_label_form(Some(eid), cx);
                                    cx.notify();
                                }))
                                .child("Sửa"),
                        )
                        .child(
                            div()
                                .id(("label-del", l.id as u64))
                                .text_size(px(13.))
                                .text_color(rgb(red))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let armed = matches!(this.dialog.as_ref(), Some(Dialog::Labels(d)) if d.delete_armed == Some(did));
                                    if armed {
                                        this.apply_label_manage(
                                            "Đã xóa thẻ phân loại",
                                            ManageOp::Delete { id: did },
                                            cx,
                                        );
                                    } else if let Some(Dialog::Labels(d)) = this.dialog.as_mut() {
                                        d.delete_armed = Some(did);
                                    }
                                    cx.notify();
                                }))
                                .child(if armed { "Bấm lần nữa để xóa" } else { "Xóa" }),
                        )
                }))
                .into_any_element(),
        };
        let editing = d.form.is_some();
        let busy = d.busy;
        div()
            .w(px(460.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_title_bar(
                if editing {
                    "Thẻ phân loại"
                } else {
                    "Quản lý thẻ phân loại"
                },
                cx,
            ))
            .child(body)
            .when_some(d.error.clone(), |el, e| {
                el.child(
                    div()
                        .px(px(16.))
                        .pb(px(8.))
                        .text_size(px(13.))
                        .text_color(rgb(red))
                        .child(e),
                )
            })
            .child(if editing {
                crate::dialogs::dialog_footer(
                    "Quay lại",
                    if busy { "Đang lưu…" } else { "Lưu" },
                    !busy,
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(Dialog::Labels(d)) = this.dialog.as_mut() {
                            d.form = None;
                        }
                        cx.notify();
                    }),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.save_label_form(cx);
                        cx.notify();
                    }),
                )
                .into_any_element()
            } else {
                crate::dialogs::dialog_footer(
                    "Đóng",
                    "Thêm thẻ",
                    !busy && !self.labels.is_empty(),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dialog = None;
                        cx.notify();
                    }),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.edit_label_form(None, cx);
                        cx.notify();
                    }),
                )
                .into_any_element()
            })
    }
}
