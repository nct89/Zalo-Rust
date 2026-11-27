//! Zalo To-Do ("Giao việc") modal backed by all 9 `/api/board/personal/*` endpoints.

use gpui::{
    div, prelude::*, px, rgb, ClickEvent, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};
use serde_json::{json, Value};

use crate::color::{border_subtle, on_accent_fg, token_rgb};
use crate::dialogs::Dialog;
use crate::fonts::{self, FAMILY_SEMIBOLD};
use crate::line_input::{LineEvent, LineInput};
use crate::shell::ShellView;

/// Filter tab inside [`TodoDlg`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TodoTab {
    /// Open / in-progress tasks (`status != 2`).
    #[default]
    Open,
    /// Completed tasks (`status == 2`).
    Done,
    /// All tasks.
    All,
}

/// Modal state for Zalo To-Do ("Giao việc").
pub struct TodoDlg {
    pub(crate) tab: TodoTab,
    pub(crate) items: Vec<zalo_client::TodoItem>,
    pub(crate) loading: bool,
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    pub(crate) show_form: bool,
    pub(crate) editing_id: Option<String>,
    pub(crate) title_input: Entity<LineInput>,
    pub(crate) desc_input: Entity<LineInput>,
    pub(crate) delete_armed: Option<String>,
}

/// Returns the toggled completion status for a To-Do item.
#[must_use]
pub fn next_todo_status(status: i64) -> i64 {
    if status == zalo_client::TODO_STATUS_DONE {
        zalo_client::TODO_STATUS_OPEN
    } else {
        zalo_client::TODO_STATUS_DONE
    }
}

/// Filters To-Do items according to the active [`TodoTab`].
#[must_use]
pub fn filter_todos(
    items: &[zalo_client::TodoItem],
    tab: TodoTab,
) -> Vec<&zalo_client::TodoItem> {
    items
        .iter()
        .filter(|it| match tab {
            TodoTab::Open => it.status != zalo_client::TODO_STATUS_DONE,
            TodoTab::Done => it.status == zalo_client::TODO_STATUS_DONE,
            TodoTab::All => true,
        })
        .collect()
}

/// Builds the `/api/board/personal/todo/update` payload for an existing task.
#[must_use]
pub fn todo_update_payload(
    existing: Option<&zalo_client::TodoItem>,
    id: &str,
    content: &str,
    description: &str,
) -> Value {
    let assignees = existing
        .map(|it| it.assignees.clone())
        .unwrap_or_default();
    let due_date = existing.map(|it| it.due_date).unwrap_or(0);
    let status = existing
        .map(|it| it.status)
        .unwrap_or(zalo_client::TODO_STATUS_OPEN);
    json!({
        "id": id,
        "content": content,
        "description": description,
        "assignees": json!(assignees).to_string(),
        "dueDate": due_date,
        "status": status,
        "extra": "{}",
        "dateDefaultType": 0,
        "watchers": [],
    })
}

/// Formats a To-Do `due_date` (unix seconds or ms) as `dd/mm HH:MM` in UTC+7,
/// or empty string when unset (`<= 0`).
#[must_use]
pub fn format_todo_due(due: i64) -> String {
    if due <= 0 {
        return String::new();
    }
    let ms = if due < 10_000_000_000 {
        due * 1000
    } else {
        due
    };
    let (_, m, d) = crate::data::vn_ymd(ms);
    let secs = (ms / 1000) + 7 * 3600;
    let hh = (secs / 3600).rem_euclid(24);
    let mm = (secs / 60).rem_euclid(60);
    format!("{d:02}/{m:02} {hh:02}:{mm:02}")
}

impl ShellView {
    /// Opens the Zalo To-Do ("Giao việc") modal and loads tasks from `/api/board/personal/list`.
    pub(crate) fn open_todo(&mut self, cx: &mut Context<Self>) {
        self.rail_popup = None;
        let title_input = cx.new(|cx| LineInput::new(cx, "Tiêu đề công việc..."));
        let desc_input = cx.new(|cx| LineInput::new(cx, "Mô tả chi tiết (không bắt buộc)..."));
        cx.subscribe(&title_input, |this, _, event: &LineEvent, cx| {
            if matches!(event, LineEvent::Submit(_)) {
                this.submit_todo_form(cx);
            }
        })
        .detach();
        cx.subscribe(&desc_input, |this, _, event: &LineEvent, cx| {
            if matches!(event, LineEvent::Submit(_)) {
                this.submit_todo_form(cx);
            }
        })
        .detach();
        self.dialog = Some(Dialog::Todo(TodoDlg {
            tab: TodoTab::Open,
            items: Vec::new(),
            loading: true,
            busy: false,
            error: None,
            show_form: false,
            editing_id: None,
            title_input,
            desc_input,
            delete_armed: None,
        }));
        self.reload_todo(cx);
    }

    /// Opens the To-Do ("Giao việc") modal with the creation form expanded and
    /// pre-filled from a chat message's text.
    pub(crate) fn open_todo_with_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.open_todo(cx);
        let initial: String = text
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
            d.show_form = true;
            if !initial.is_empty() {
                d.title_input
                    .update(cx, |input, cx| input.set_text(initial, cx));
            }
        }
    }

    /// Reloads personal To-Do tasks (`/api/board/personal/list`), verifies sync
    /// (`/api/board/personal/todo/verify`), and acknowledges pending dings
    /// (`/api/board/personal/todo/dingack`).
    pub(crate) fn reload_todo(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
                d.loading = false;
                d.error = Some("Chưa đăng nhập phiên Zalo".into());
            }
            return;
        };
        if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
            d.loading = true;
            d.error = None;
        }
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let mut all_items: Vec<zalo_client::TodoItem> = Vec::new();
                        let mut deleted: std::collections::HashSet<String> =
                            std::collections::HashSet::new();
                        let mut scroll_id = String::new();
                        let mut verify_ok = false;
                        for _ in 0..5 {
                            match api.todo_verify(&scroll_id, 0).await {
                                Ok(val) => {
                                    verify_ok = true;
                                    let root =
                                        val.get("data").filter(|v| v.is_object()).unwrap_or(&val);
                                    let page_items = zalo_client::parse_todo_list(&val);
                                    let page_del: Vec<String> = root
                                        .get("deleted")
                                        .and_then(Value::as_array)
                                        .map(|arr| {
                                            arr.iter()
                                                .filter_map(|x| {
                                                    x.as_str()
                                                        .map(str::to_owned)
                                                        .or_else(|| x.as_i64().map(|n| n.to_string()))
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    let next_scroll = root
                                        .get("scrollId")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_owned();
                                    let empty_page = page_items.is_empty() && page_del.is_empty();
                                    all_items.extend(page_items);
                                    deleted.extend(page_del);
                                    if empty_page || next_scroll.is_empty() || next_scroll == scroll_id
                                    {
                                        break;
                                    }
                                    scroll_id = next_scroll;
                                }
                                Err(err) => {
                                    if !verify_ok {
                                        // Fallback to `/api/board/personal/list` with Zalo PC query shape.
                                        let q = json!({ "status": { "lt": 2 } });
                                        let list_val = api
                                            .todo_list(
                                                "",
                                                &q,
                                                0,
                                                50,
                                                zalo_client::TODO_TYPE_DEFAULT,
                                            )
                                            .await
                                            .map_err(|_| err.to_string())?;
                                        all_items = zalo_client::parse_todo_list(&list_val);
                                    }
                                    break;
                                }
                            }
                        }
                        if !deleted.is_empty() {
                            all_items.retain(|it| !deleted.contains(&it.id));
                        }
                        // Deduplicate by id keeping the latest updated entry.
                        let mut seen = std::collections::HashSet::new();
                        all_items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                        all_items.retain(|it| it.id.is_empty() || seen.insert(it.id.clone()));
                        let ding_ids: Vec<String> = all_items
                            .iter()
                            .filter(|it| {
                                it.ding_count > 0
                                    && it.status != zalo_client::TODO_STATUS_DONE
                                    && !it.id.is_empty()
                            })
                            .map(|it| it.id.clone())
                            .collect();
                        if !ding_ids.is_empty() {
                            let _ = api.todo_ding_ack(&ding_ids).await;
                        }
                        Ok::<Vec<zalo_client::TodoItem>, String>(all_items)
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(Dialog::Todo(d)) = view.dialog.as_mut() {
                    d.loading = false;
                    match res {
                        Ok(items) => {
                            d.items = items;
                            d.error = None;
                        }
                        Err(e) => {
                            d.error = Some(if e.contains("-69") {
                                "Máy chủ Zalo chưa kích hoạt Zalo To-Do cho tài khoản này (mã -69).".into()
                            } else {
                                e
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_create_todo(&mut self, cx: &mut Context<Self>) {
        if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
            d.show_form = true;
            d.editing_id = None;
            d.error = None;
            d.title_input.update(cx, |inp, cx| inp.set_text(String::new(), cx));
            d.desc_input.update(cx, |inp, cx| inp.set_text(String::new(), cx));
        }
    }

    fn start_edit_todo(&mut self, id: String, cx: &mut Context<Self>) {
        let cached = match self.dialog.as_ref() {
            Some(Dialog::Todo(d)) => d.items.iter().find(|it| it.id == id).cloned(),
            _ => None,
        };
        if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
            d.show_form = true;
            d.editing_id = Some(id.clone());
            d.error = None;
            if let Some(it) = cached.as_ref() {
                d.title_input
                    .update(cx, |inp, cx| inp.set_text(it.content.clone(), cx));
                d.desc_input
                    .update(cx, |inp, cx| inp.set_text(it.description.clone(), cx));
            }
        }
        // Fetch fresh detail via `/api/board/personal/get` to ensure latest server state.
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        let target_id = id;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let val = api.todo_get(&target_id).await.map_err(|e| e.to_string())?;
                        Ok::<Option<zalo_client::TodoItem>, String>(
                            zalo_client::parse_todo_list(&val).into_iter().next(),
                        )
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Ok(Some(fresh)) = res {
                    if let Some(Dialog::Todo(d)) = view.dialog.as_mut() {
                        if d.editing_id.as_deref() == Some(fresh.id.as_str()) {
                            d.title_input
                                .update(cx, |inp, cx| inp.set_text(fresh.content.clone(), cx));
                            d.desc_input.update(cx, |inp, cx| {
                                inp.set_text(fresh.description.clone(), cx)
                            });
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn submit_todo_form(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Todo(d)) = self.dialog.as_ref() else {
            return;
        };
        if d.busy {
            return;
        }
        let content = d.title_input.read(cx).text().trim().to_owned();
        let description = d.desc_input.read(cx).text().trim().to_owned();
        if content.is_empty() {
            if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
                d.error = Some("Vui lòng nhập tiêu đề công việc".into());
            }
            cx.notify();
            return;
        }
        let editing_id = d.editing_id.clone();
        let existing = editing_id
            .as_ref()
            .and_then(|eid| d.items.iter().find(|it| &it.id == eid).cloned());
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
                d.error = Some("Chưa đăng nhập phiên Zalo".into());
            }
            cx.notify();
            return;
        };
        let me = self.self_uid().unwrap_or_default().to_owned();
        if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
            d.busy = true;
            d.error = None;
        }
        cx.spawn(async move |this, cx| {
            let is_edit = editing_id.is_some();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        if let Some(eid) = editing_id {
                            let payload = todo_update_payload(
                                existing.as_ref(),
                                &eid,
                                &content,
                                &description,
                            );
                            api.todo_update(&payload)
                                .await
                                .map_err(|e| e.to_string())?;
                        } else {
                            let assignees = if me.is_empty() {
                                Vec::new()
                            } else {
                                vec![me]
                            };
                            let input = zalo_client::TodoCreateInput {
                                content,
                                description,
                                assignees,
                                due_date: 0,
                                status: zalo_client::TODO_STATUS_OPEN,
                                extra: json!({}),
                            };
                            api.todo_create(&input, -1)
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                        Ok::<(), String>(())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match res {
                    Ok(()) => {
                        if let Some(Dialog::Todo(d)) = view.dialog.as_mut() {
                            d.busy = false;
                            d.show_form = false;
                            d.editing_id = None;
                            d.error = None;
                        }
                        view.set_note(if is_edit {
                            "Đã cập nhật công việc"
                        } else {
                            "Đã tạo công việc mới"
                        });
                        view.reload_todo(cx);
                    }
                    Err(e) => {
                        if let Some(Dialog::Todo(d)) = view.dialog.as_mut() {
                            d.busy = false;
                            d.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_todo_status(&mut self, id: String, current_status: i64, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        let next = next_todo_status(current_status);
        if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
            if let Some(it) = d.items.iter_mut().find(|it| it.id == id) {
                it.status = next;
            }
            d.error = None;
        }
        let target = id;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.todo_update_status(&target, next)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match res {
                    Ok(_) => {
                        view.set_note(if next == zalo_client::TODO_STATUS_DONE {
                            "Đã đánh dấu hoàn thành"
                        } else {
                            "Đã mở lại công việc"
                        });
                    }
                    Err(e) => {
                        if let Some(Dialog::Todo(d)) = view.dialog.as_mut() {
                            d.error = Some(e);
                        }
                        view.reload_todo(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ding_todo_item(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        let target = id;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.todo_ding(&[target])
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match res {
                    Ok(_) => view.set_note("Đã gửi nhắc việc (Ding)"),
                    Err(e) => {
                        if let Some(Dialog::Todo(d)) = view.dialog.as_mut() {
                            d.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_todo_item(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        if let Some(Dialog::Todo(d)) = self.dialog.as_mut() {
            d.delete_armed = None;
            d.busy = true;
            d.error = None;
        }
        let target = id.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.todo_delete(&target).await.map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(Dialog::Todo(d)) = view.dialog.as_mut() {
                    d.busy = false;
                    match res {
                        Ok(_) => {
                            d.items.retain(|it| it.id != id);
                            view.set_note("Đã xóa công việc");
                        }
                        Err(e) => {
                            d.error = Some(if e.contains("-69") {
                                "Máy chủ Zalo chưa kích hoạt Zalo To-Do cho tài khoản này (mã -69).".into()
                            } else {
                                e
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Renders the Zalo To-Do ("Giao việc") modal.
    pub(crate) fn todo_el(&self, d: &TodoDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = token_rgb("--layer-background", "#ffffff");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let accent = token_rgb("--B55", "#0068ff");
        let accent_fg = on_accent_fg();
        let danger = token_rgb("--R60", "#c31818");
        let link = crate::color::link_fg();
        let line = border_subtle();

        let open_count = d
            .items
            .iter()
            .filter(|it| it.status != zalo_client::TODO_STATUS_DONE)
            .count();
        let done_count = d.items.len().saturating_sub(open_count);
        let filtered = filter_todos(&d.items, d.tab);
        let tabs: [(TodoTab, String); 3] = [
            (TodoTab::Open, format!("Cần làm ({open_count})")),
            (TodoTab::Done, format!("Đã xong ({done_count})")),
            (TodoTab::All, format!("Tất cả ({})", d.items.len())),
        ];

        div()
            .w(px(540.))
            .max_h(px(620.))
            .rounded(px(8.))
            .bg(rgb(bg))
            .border_1()
            .border_color(line)
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_title_bar("Zalo To-Do · Giao việc", cx))
            .child(
                // Tab bar + actions ("+ Giao việc", "Tải lại")
                div()
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(line)
                    .child(div().flex().flex_row().items_center().gap(px(6.)).children(
                        tabs.into_iter().enumerate().map(|(i, (tab, label))| {
                            let active = d.tab == tab;
                            div()
                                .id(("todo-tab", i as u64))
                                .h(px(30.))
                                .px(px(10.))
                                .rounded(px(15.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_size(px(13.))
                                .font_family(FAMILY_SEMIBOLD)
                                .bg(rgb(if active { accent } else { subtle }))
                                .text_color(rgb(if active { accent_fg } else { ink }))
                                .when(!active, |el| el.hover(move |st| st.bg(rgb(hover))))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    if let Some(Dialog::Todo(dlg)) = this.dialog.as_mut() {
                                        dlg.tab = tab;
                                        dlg.delete_armed = None;
                                    }
                                    cx.notify();
                                }))
                                .child(label)
                        }),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("todo-reload")
                                    .h(px(30.))
                                    .px(px(10.))
                                    .rounded(px(4.))
                                    .bg(rgb(subtle))
                                    .hover(move |st| st.bg(rgb(hover)))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .text_size(px(12.5))
                                    .text_color(rgb(ink))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.reload_todo(cx);
                                        cx.notify();
                                    }))
                                    .child(if d.loading { "Đang tải…" } else { "Tải lại" }),
                            )
                            .child(
                                div()
                                    .id("todo-add-toggle")
                                    .h(px(30.))
                                    .px(px(12.))
                                    .rounded(px(4.))
                                    .bg(rgb(accent))
                                    .text_color(rgb(accent_fg))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        let open = matches!(
                                            this.dialog.as_ref(),
                                            Some(Dialog::Todo(dlg)) if dlg.show_form && dlg.editing_id.is_none()
                                        );
                                        if open {
                                            if let Some(Dialog::Todo(dlg)) = this.dialog.as_mut() {
                                                dlg.show_form = false;
                                            }
                                        } else {
                                            this.start_create_todo(cx);
                                        }
                                        cx.notify();
                                    }))
                                    .child("+ Giao việc"),
                            ),
                    ),
            )
            .when(d.show_form, |el| {
                let is_edit = d.editing_id.is_some();
                let busy = d.busy;
                el.child(
                    div()
                        .flex()
                        .flex_none()
                        .flex_col()
                        .gap(px(8.))
                        .px(px(16.))
                        .py(px(12.))
                        .bg(rgb(subtle))
                        .border_b_1()
                        .border_color(line)
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(ink))
                                .child(if is_edit {
                                    "Chỉnh sửa công việc"
                                } else {
                                    "Tạo công việc mới"
                                }),
                        )
                        .child(
                            div()
                                .h(px(36.))
                                .px(px(10.))
                                .rounded(px(4.))
                                .bg(rgb(bg))
                                .border_1()
                                .border_color(line)
                                .flex()
                                .items_center()
                                .child(d.title_input.clone()),
                        )
                        .child(
                            div()
                                .h(px(36.))
                                .px(px(10.))
                                .rounded(px(4.))
                                .bg(rgb(bg))
                                .border_1()
                                .border_color(line)
                                .flex()
                                .items_center()
                                .child(d.desc_input.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .id("todo-form-cancel")
                                        .h(px(32.))
                                        .px(px(12.))
                                        .rounded(px(4.))
                                        .bg(rgb(bg))
                                        .border_1()
                                        .border_color(line)
                                        .hover(move |st| st.bg(rgb(hover)))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .text_size(px(13.))
                                        .text_color(rgb(ink))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            if let Some(Dialog::Todo(dlg)) = this.dialog.as_mut() {
                                                dlg.show_form = false;
                                                dlg.editing_id = None;
                                                dlg.error = None;
                                            }
                                            cx.notify();
                                        }))
                                        .child("Hủy"),
                                )
                                .child(
                                    div()
                                        .id("todo-form-save")
                                        .h(px(32.))
                                        .px(px(14.))
                                        .rounded(px(4.))
                                        .bg(rgb(accent))
                                        .text_color(rgb(accent_fg))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .when(!busy, |b| {
                                            b.cursor_pointer().on_click(cx.listener(
                                                |this, _: &ClickEvent, _, cx| {
                                                    this.submit_todo_form(cx);
                                                },
                                            ))
                                        })
                                        .text_size(px(13.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .child(if busy {
                                            "Đang lưu…"
                                        } else if is_edit {
                                            "Lưu thay đổi"
                                        } else {
                                            "Tạo việc"
                                        }),
                                ),
                        ),
                )
            })
            .when_some(d.error.clone(), |el, err| {
                el.child(
                    div()
                        .flex_none()
                        .px(px(16.))
                        .py(px(6.))
                        .text_size(px(12.5))
                        .text_color(rgb(danger))
                        .child(err),
                )
            })
            .child(
                div()
                    .id("todo-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(240.))
                    .max_h(px(420.))
                    .overflow_y_scroll()
                    .when(d.loading && filtered.is_empty(), |el| {
                        el.child(
                            div()
                                .p(px(24.))
                                .text_center()
                                .text_size(px(13.5))
                                .text_color(rgb(muted))
                                .child("Đang tải danh sách công việc…"),
                        )
                    })
                    .when(!d.loading && filtered.is_empty(), |el| {
                        el.child(
                            div()
                                .p(px(28.))
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .text_color(rgb(muted))
                                        .child(fonts::icon_el('\u{EDB8}', "☑", 34.)),
                                )
                                .child(
                                    div()
                                        .text_size(px(13.5))
                                        .text_color(rgb(muted))
                                        .child("Chưa có công việc nào trong mục này."),
                                ),
                        )
                    })
                    .children(filtered.into_iter().enumerate().map(|(i, item)| {
                        let done = item.status == zalo_client::TODO_STATUS_DONE;
                        let id_toggle = item.id.clone();
                        let id_ding = item.id.clone();
                        let id_edit = item.id.clone();
                        let id_del = item.id.clone();
                        let status = item.status;
                        let armed = d.delete_armed.as_deref() == Some(item.id.as_str());
                        let due = format_todo_due(item.due_date);

                        div()
                            .id(("todo-row", i as u64))
                            .flex()
                            .flex_none()
                            .flex_row()
                            .items_center()
                            .gap(px(10.))
                            .px(px(16.))
                            .py(px(10.))
                            .border_b_1()
                            .border_color(line)
                            .hover(move |st| st.bg(rgb(hover)))
                            .child(
                                div()
                                    .id(("todo-check", i as u64))
                                    .size(px(20.))
                                    .flex_none()
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(if done { rgb(accent).into() } else { line })
                                    .when(done, |el| el.bg(rgb(accent)).text_color(rgb(accent_fg)))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.toggle_todo_status(id_toggle.clone(), status, cx);
                                        cx.notify();
                                    }))
                                    .when(done, |el| {
                                        el.child(fonts::icon_el('\u{EA7B}', "✓", 13.))
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(if done { muted } else { ink }))
                                            .truncate()
                                            .child(item.content.clone()),
                                    )
                                    .when(!item.description.is_empty(), |el| {
                                        el.child(
                                            div()
                                                .text_size(px(12.5))
                                                .text_color(rgb(muted))
                                                .truncate()
                                                .child(item.description.clone()),
                                        )
                                    })
                                    .when(!due.is_empty() || item.ding_count > 0, |el| {
                                        let meta = match (due.is_empty(), item.ding_count > 0) {
                                            (false, true) => {
                                                format!("Hạn: {due} · Đã nhắc {} lần", item.ding_count)
                                            }
                                            (false, false) => format!("Hạn: {due}"),
                                            (true, true) => format!("Đã nhắc {} lần", item.ding_count),
                                            (true, false) => String::new(),
                                        };
                                        el.child(
                                            div()
                                                .text_size(px(11.5))
                                                .text_color(rgb(muted))
                                                .child(meta),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.))
                                    .when(!done, |el| {
                                        el.child(
                                            div()
                                                .id(("todo-ding", i as u64))
                                                .px(px(6.))
                                                .py(px(3.))
                                                .rounded(px(4.))
                                                .bg(rgb(subtle))
                                                .hover(move |st| st.bg(rgb(hover)))
                                                .cursor_pointer()
                                                .text_size(px(12.))
                                                .text_color(rgb(ink))
                                                .on_click(cx.listener(
                                                    move |this, _: &ClickEvent, _, cx| {
                                                        this.ding_todo_item(id_ding.clone(), cx);
                                                        cx.notify();
                                                    },
                                                ))
                                                .child("Nhắc"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id(("todo-edit", i as u64))
                                            .cursor_pointer()
                                            .text_size(px(12.5))
                                            .text_color(rgb(link))
                                            .on_click(cx.listener(
                                                move |this, _: &ClickEvent, _, cx| {
                                                    this.start_edit_todo(id_edit.clone(), cx);
                                                    cx.notify();
                                                },
                                            ))
                                            .child("Sửa"),
                                    )
                                    .child(
                                        div()
                                            .id(("todo-del", i as u64))
                                            .cursor_pointer()
                                            .text_size(px(12.5))
                                            .text_color(rgb(danger))
                                            .on_click(cx.listener(
                                                move |this, _: &ClickEvent, _, cx| {
                                                    let is_armed = matches!(
                                                        this.dialog.as_ref(),
                                                        Some(Dialog::Todo(dlg))
                                                            if dlg.delete_armed.as_deref() == Some(id_del.as_str())
                                                    );
                                                    if is_armed {
                                                        this.delete_todo_item(id_del.clone(), cx);
                                                    } else if let Some(Dialog::Todo(dlg)) =
                                                        this.dialog.as_mut()
                                                    {
                                                        dlg.delete_armed = Some(id_del.clone());
                                                    }
                                                    cx.notify();
                                                },
                                            ))
                                            .child(if armed { "Xác nhận?" } else { "Xóa" }),
                                    ),
                            )
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_todo(id: &str, status: i64) -> zalo_client::TodoItem {
        zalo_client::TodoItem {
            id: id.into(),
            content: format!("Task {id}"),
            description: "Desc".into(),
            status,
            due_date: 0,
            updated_at: 100,
            creator_id: "1".into(),
            assignees: vec!["1".into()],
            ding_count: 0,
        }
    }

    #[test]
    fn filter_todos_splits_open_done_and_all() {
        let items = vec![
            sample_todo("1", zalo_client::TODO_STATUS_OPEN),
            sample_todo("2", zalo_client::TODO_STATUS_DONE),
            sample_todo("3", zalo_client::TODO_STATUS_OPEN),
        ];
        assert_eq!(filter_todos(&items, TodoTab::Open).len(), 2);
        assert_eq!(filter_todos(&items, TodoTab::Done).len(), 1);
        assert_eq!(filter_todos(&items, TodoTab::All).len(), 3);
    }

    #[test]
    fn next_todo_status_toggles_open_and_done() {
        assert_eq!(
            next_todo_status(zalo_client::TODO_STATUS_OPEN),
            zalo_client::TODO_STATUS_DONE
        );
        assert_eq!(
            next_todo_status(zalo_client::TODO_STATUS_DONE),
            zalo_client::TODO_STATUS_OPEN
        );
    }

    #[test]
    fn todo_update_payload_preserves_metadata() {
        let mut item = sample_todo("99", zalo_client::TODO_STATUS_OPEN);
        item.due_date = 1_700_000_000_000;
        let payload = todo_update_payload(Some(&item), "99", "New title", "New desc");
        assert_eq!(payload["id"], "99");
        assert_eq!(payload["content"], "New title");
        assert_eq!(payload["description"], "New desc");
        assert_eq!(payload["dueDate"], 1_700_000_000_000i64);
        assert_eq!(payload["status"], zalo_client::TODO_STATUS_OPEN);
    }

    #[test]
    fn format_todo_due_handles_zero_and_timestamps() {
        assert_eq!(format_todo_due(0), "");
        assert_eq!(format_todo_due(-1), "");
        assert!(!format_todo_due(1_700_000_000).is_empty());
        assert!(!format_todo_due(1_700_000_000_000).is_empty());
    }
}
