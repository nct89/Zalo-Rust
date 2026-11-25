//! "Auto-Reply" (Settings › Messages): the account's rules through
//! `/api/autoreply/{list,create,update,delete}`, ported from zalo-gui.

use gpui::{
    div, prelude::*, px, rgb, ClickEvent, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};
use serde_json::Value;

use crate::color::token_rgb;
use crate::dialogs::Dialog;
use crate::line_input::LineInput;
use crate::shell::ShellView;

/// One rule. Times are unix milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rule {
    /// Server id.
    pub id: i64,
    /// Switch.
    pub enable: bool,
    /// Reply text.
    pub content: String,
    /// Start.
    pub start_ms: i64,
    /// End.
    pub end_ms: i64,
    /// `RRULE:FREQ=DAILY;` when it repeats.
    pub recurrence: Vec<String>,
    /// 0 everyone, 1 strangers, else picked contacts.
    pub scope: i64,
}

impl Rule {
    fn daily(&self) -> bool {
        self.recurrence.iter().any(|r| r.contains("FREQ=DAILY"))
    }
}

/// Rules of `/api/autoreply/list` (`item`).
#[must_use]
pub fn rules_from(value: &Value) -> Vec<Rule> {
    let root = value.get("data").unwrap_or(value);
    root.get("item")
        .or_else(|| root.get("items"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|it| {
            Some(Rule {
                id: it.get("id").and_then(Value::as_i64)?,
                enable: it.get("enable").and_then(Value::as_bool).unwrap_or(false),
                content: it
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                start_ms: it.get("startTime").and_then(Value::as_i64).unwrap_or(0),
                end_ms: it.get("endTime").and_then(Value::as_i64).unwrap_or(0),
                recurrence: it
                    .get("recurrence")
                    .and_then(Value::as_array)
                    .map(|r| {
                        r.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                scope: it.get("scope").and_then(Value::as_i64).unwrap_or(0),
            })
        })
        .collect()
}

/// `dd/mm/yyyy hh:mm` in Vietnam time.
#[must_use]
pub fn format_vn(ms: i64) -> String {
    let (y, m, d) = crate::data::vn_ymd(ms);
    let tod = (ms + 7 * 3_600_000).rem_euclid(86_400_000);
    format!(
        "{d:02}/{m:02}/{y} {:02}:{:02}",
        tod / 3_600_000,
        (tod % 3_600_000) / 60_000
    )
}

/// `dd/mm/yyyy hh:mm` (Vietnam time) → unix ms.
#[must_use]
pub fn parse_vn(text: &str) -> Option<i64> {
    let (date, clock) = text.trim().split_once(' ')?;
    let mut d = date.split('/').map(|p| p.trim().parse::<i64>().ok());
    let (day, month, year) = (d.next()??, d.next()??, d.next()??);
    let (hour, minute) = clock.trim().split_once(':')?;
    let (hour, minute) = (hour.parse::<i64>().ok()?, minute.parse::<i64>().ok()?);
    if !(1..=31).contains(&day) || !(1..=12).contains(&month) || hour > 23 || minute > 59 {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400_000 + hour * 3_600_000 + minute * 60_000 - 7 * 3_600_000)
}

fn time_range(r: &Rule) -> String {
    if r.daily() {
        let clock = |ms: i64| {
            format_vn(ms)
                .split_once(' ')
                .map(|(_, c)| c.to_owned())
                .unwrap_or_default()
        };
        format!("Hằng ngày {} - {}", clock(r.start_ms), clock(r.end_ms))
    } else {
        format!("{} - {}", format_vn(r.start_ms), format_vn(r.end_ms))
    }
}

/// Editor of a new (`id` 0) or existing rule.
pub struct Form {
    id: i64,
    enable: bool,
    daily: bool,
    scope: i64,
    content: Entity<LineInput>,
    start: Entity<LineInput>,
    end: Entity<LineInput>,
}

/// Dialog state.
pub struct AutoReplyDlg {
    list: Option<Result<Vec<Rule>, String>>,
    busy: bool,
    form: Option<Form>,
    error: Option<String>,
}

impl ShellView {
    /// Settings › Messages › Auto-Reply › "Open".
    pub(crate) fn open_auto_reply(&mut self, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::AutoReply(AutoReplyDlg {
            list: None,
            busy: true,
            form: None,
            error: None,
        }));
        self.write_rules(None, None, cx);
    }

    /// Save (`Some`) and/or delete, then reload the list.
    fn write_rules(&mut self, save: Option<Rule>, delete: Option<i64>, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            if let Some(Dialog::AutoReply(d)) = self.dialog.as_mut() {
                d.busy = false;
                d.list = Some(Err("Chưa đăng nhập".into()));
            }
            return;
        };
        if let Some(Dialog::AutoReply(d)) = self.dialog.as_mut() {
            d.busy = true;
        }
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        if let Some(id) = delete {
                            api.delete_auto_reply(id).await.map_err(|e| e.to_string())?;
                        }
                        if let Some(r) = save {
                            api.save_auto_reply(
                                (r.id != 0).then_some(r.id),
                                r.enable,
                                &r.content,
                                r.start_ms,
                                r.end_ms,
                                &r.recurrence,
                                r.scope,
                                &[],
                            )
                            .await
                            .map_err(|e| e.to_string())?;
                        }
                        api.get_auto_replies()
                            .await
                            .map(|v| rules_from(&v))
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(Dialog::AutoReply(d)) = view.dialog.as_mut() {
                    d.busy = false;
                    match got {
                        Ok(list) => {
                            d.list = Some(Ok(list));
                            d.form = None;
                            d.error = None;
                        }
                        Err(e) if d.form.is_some() => d.error = Some(e),
                        Err(e) => d.list = Some(Err(e)),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn edit_rule(&mut self, rule: Option<Rule>, cx: &mut Context<Self>) {
        let now = crate::data::now_ms();
        let r = rule.unwrap_or(Rule {
            enable: true,
            start_ms: now,
            end_ms: now + 3_600_000,
            ..Rule::default()
        });
        let mk = |cx: &mut Context<Self>, ph: &str, text: String| {
            cx.new(|cx| {
                let mut i = LineInput::new(cx, ph.to_owned());
                i.set_text(text, cx);
                i
            })
        };
        let content = mk(
            cx,
            "Nội dung trả lời (vd: Mình đang bận, sẽ trả lời sau)",
            r.content.clone(),
        );
        let start = mk(cx, "dd/mm/yyyy hh:mm", format_vn(r.start_ms));
        let end = mk(cx, "dd/mm/yyyy hh:mm", format_vn(r.end_ms));
        if let Some(Dialog::AutoReply(d)) = self.dialog.as_mut() {
            d.error = None;
            d.form = Some(Form {
                id: r.id,
                enable: r.enable,
                daily: r.daily(),
                scope: r.scope,
                content,
                start,
                end,
            });
        }
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::AutoReply(d)) = self.dialog.as_ref() else {
            return;
        };
        let Some(f) = d.form.as_ref() else {
            return;
        };
        let content = f.content.read(cx).text().trim().to_owned();
        let start = parse_vn(&f.start.read(cx).text());
        let end = parse_vn(&f.end.read(cx).text());
        let error = if content.is_empty() {
            Some("Nhập nội dung trả lời")
        } else if start.is_none() || end.is_none() {
            Some("Thời gian theo dạng dd/mm/yyyy hh:mm")
        } else if end <= start {
            Some("Thời gian kết thúc phải sau thời gian bắt đầu")
        } else {
            None
        };
        if let Some(e) = error {
            if let Some(Dialog::AutoReply(d)) = self.dialog.as_mut() {
                d.error = Some(e.into());
            }
            return;
        }
        let rule = Rule {
            id: f.id,
            enable: f.enable,
            content,
            start_ms: start.unwrap_or(0),
            end_ms: end.unwrap_or(0),
            recurrence: if f.daily {
                vec!["RRULE:FREQ=DAILY;".into()]
            } else {
                Vec::new()
            },
            scope: f.scope,
        };
        self.write_rules(Some(rule), None, cx);
    }

    pub(crate) fn auto_reply_el(
        &self,
        d: &AutoReplyDlg,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let red = token_rgb("--R60", "#c31818");
        let line = crate::color::border_subtle();
        let body = match (&d.form, &d.list) {
            (Some(f), _) => {
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
                let check = |key: &'static str, on: bool, label: &'static str| {
                    div()
                        .id(key)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .text_size(px(14.))
                        .child(
                            div()
                                .size(px(18.))
                                .rounded(px(3.))
                                .border_1()
                                .border_color(if on {
                                    rgb(blue)
                                } else {
                                    rgb(token_rgb("--text-disabled", "#8f9bad"))
                                })
                                .bg(if on {
                                    rgb(blue)
                                } else {
                                    rgb(token_rgb("--layer-background", "#ffffff"))
                                })
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(crate::color::on_accent_fg()))
                                .text_size(px(12.))
                                .child(if on { "✓" } else { "" }),
                        )
                        .child(label)
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .p(px(16.))
                    .child(field("Nội dung", f.content.clone()))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(12.))
                            .child(div().flex_1().child(field("Bắt đầu", f.start.clone())))
                            .child(div().flex_1().child(field("Kết thúc", f.end.clone()))),
                    )
                    .child(
                        check("ar-daily", f.daily, "Lặp lại hằng ngày (chỉ dùng giờ)").on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(Dialog::AutoReply(AutoReplyDlg {
                                    form: Some(f), ..
                                })) = this.dialog.as_mut()
                                {
                                    f.daily = !f.daily;
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        check(
                            "ar-strangers",
                            f.scope == 1,
                            "Chỉ trả lời người lạ (không phải bạn bè)",
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                if let Some(Dialog::AutoReply(AutoReplyDlg {
                                    form: Some(f), ..
                                })) = this.dialog.as_mut()
                                {
                                    f.scope = if f.scope == 1 { 0 } else { 1 };
                                }
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        check("ar-enable", f.enable, "Bật quy tắc này").on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                if let Some(Dialog::AutoReply(AutoReplyDlg {
                                    form: Some(f), ..
                                })) = this.dialog.as_mut()
                                {
                                    f.enable = !f.enable;
                                }
                                cx.notify();
                            },
                        )),
                    )
                    .into_any_element()
            }
            (None, None) => div()
                .p(px(16.))
                .text_size(px(14.))
                .text_color(rgb(muted))
                .child("Đang tải…")
                .into_any_element(),
            (None, Some(Err(e))) => div()
                .p(px(16.))
                .text_size(px(14.))
                .text_color(rgb(muted))
                .child(e.clone())
                .into_any_element(),
            (None, Some(Ok(list))) => div()
                .id("ar-list")
                .flex()
                .flex_col()
                .max_h(px(420.))
                .overflow_y_scroll()
                .when(list.is_empty(), |el| {
                    el.child(
                        div()
                            .p(px(16.))
                            .text_size(px(14.))
                            .text_color(rgb(muted))
                            .child("Chưa có tin trả lời tự động nào"),
                    )
                })
                .children(list.iter().cloned().map(|r| {
                    let (toggle, edit, del) = (r.clone(), r.clone(), r.id);
                    div()
                        .id(("ar-rule", r.id as u64))
                        .flex()
                        .flex_none()
                        .flex_row()
                        .items_center()
                        .gap(px(12.))
                        .px(px(16.))
                        .py(px(12.))
                        .border_b_1()
                        .border_color(line)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w(px(0.))
                                .gap(px(4.))
                                .child(div().text_size(px(14.)).truncate().child(r.content.clone()))
                                .child(div().text_size(px(12.)).text_color(rgb(muted)).child(
                                    format!(
                                        "{} · {}",
                                        time_range(&r),
                                        match r.scope {
                                            0 => "Tất cả mọi người",
                                            1 => "Người lạ",
                                            _ => "Liên hệ đã chọn",
                                        }
                                    ),
                                )),
                        )
                        .child(
                            div()
                                .id(("ar-toggle", r.id as u64))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let mut r = toggle.clone();
                                    r.enable = !r.enable;
                                    this.write_rules(Some(r), None, cx);
                                    cx.notify();
                                }))
                                .child(crate::settings::switch_el(r.enable)),
                        )
                        .child(
                            div()
                                .id(("ar-edit", r.id as u64))
                                .text_size(px(13.))
                                .text_color(rgb(blue))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.edit_rule(Some(edit.clone()), cx);
                                    cx.notify();
                                }))
                                .child("Sửa"),
                        )
                        .child(
                            div()
                                .id(("ar-del", r.id as u64))
                                .text_size(px(13.))
                                .text_color(rgb(red))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.write_rules(None, Some(del), cx);
                                    cx.notify();
                                }))
                                .child("Xóa"),
                        )
                }))
                .into_any_element(),
        };
        let editing = d.form.is_some();
        div()
            .w(px(520.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_title_bar(
                if editing {
                    "Tin trả lời tự động"
                } else {
                    "Trả lời tự động"
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
            .child(if editing {
                crate::dialogs::dialog_footer(
                    "Quay lại",
                    if d.busy { "Đang lưu…" } else { "Lưu" },
                    !d.busy,
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(Dialog::AutoReply(d)) = this.dialog.as_mut() {
                            d.form = None;
                            d.error = None;
                        }
                        cx.notify();
                    }),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.save_form(cx);
                        cx.notify();
                    }),
                )
                .into_any_element()
            } else {
                crate::dialogs::dialog_footer(
                    "Đóng",
                    "Tạo mới",
                    !d.busy && d.list.as_ref().is_some_and(Result::is_ok),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dialog = None;
                        cx.notify();
                    }),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.edit_rule(None, cx);
                        cx.notify();
                    }),
                )
                .into_any_element()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_and_times() {
        let rules = rules_from(&serde_json::json!({"item": [{
            "id": 7, "enable": true, "content": "Bận", "startTime": 1_000, "endTime": 2_000,
            "recurrence": ["RRULE:FREQ=DAILY;"], "scope": 0
        }]}));
        assert_eq!(rules.len(), 1);
        assert!(rules[0].daily());
        let ms = parse_vn("12/11/2026 08:30").unwrap();
        assert_eq!(format_vn(ms), "12/11/2026 08:30");
        assert!(parse_vn("32/11/2026 08:30").is_none());
    }
}
