//! Group board (notes, reminders, polls, pinned messages) and 1-1 reminders.

use std::collections::HashSet;

use gpui::{
    div, prelude::*, px, rgb, ClickEvent, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};
use serde_json::Value;

use crate::color::{border_subtle, str_vi, token_rgb};
use crate::dialogs::dialog_footer;
use crate::fonts::FAMILY_SEMIBOLD;
use crate::line_input::LineInput;
use crate::shell::ShellView;

/// Kind of board item returned by `/api/board/list` or `/api/board/oneone/list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardKind {
    /// Text note (`boardType` 1 / topic type 0).
    Note,
    /// Scheduled reminder (`topic type 4` / oneone reminder).
    Reminder,
    /// Group poll (`boardType` 3).
    Poll,
    /// Pinned chat message (`boardType` 2).
    PinnedMsg,
}

/// Active view inside [`BoardDlg`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BoardTab {
    /// Show all board entries.
    #[default]
    All,
    /// Show reminders only.
    Reminders,
    /// Show notes and pinned messages.
    Notes,
    /// Show polls.
    Polls,
    /// Inline create-reminder form.
    CreateReminder,
    /// Inline create-note form.
    CreateNote,
    /// Inline create-poll form.
    CreatePoll,
}

/// One option inside a poll card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollOption {
    /// Option identifier (`option_id`).
    pub id: i64,
    /// Option label (`content`).
    pub content: String,
    /// Number of votes on this option.
    pub votes: i64,
    /// Whether the current user voted for this option.
    pub voted: bool,
}

/// Parsed item from Zalo's group or 1-on-1 board.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardItem {
    /// Server item id (`id`, `topicId`, `poll_id`, or `reminderId`).
    pub id: String,
    /// Category.
    pub kind: BoardKind,
    /// Main title, message text, or poll question.
    pub title: String,
    /// Optional creator / metadata subtitle.
    pub subtitle: String,
    /// Due timestamp in unix seconds or ms (for reminders).
    pub at_ms: i64,
    /// Numeric poll id when `kind == BoardKind::Poll`.
    pub poll_id: Option<i64>,
    /// Numeric board item type (`pinv2` type).
    pub pin_type: i64,
    /// Whether pinned on top bar.
    pub pinned: bool,
    /// Poll options (for polls).
    pub options: Vec<PollOption>,
    /// True if poll is closed (`status != 0`).
    pub closed: bool,
    /// Topic type sent back to `updatev2` (`0` note, `4` reminder).
    pub topic_type: i64,
    /// Repeat mode (`0` none, `1` daily, `2` weekly, `3` monthly).
    pub repeat: i64,
    /// `responseMem.myResp`: `0` none, `1` accept, `2` reject.
    pub my_resp: i64,
}

/// Modal state for group board (notes, reminders, polls) & 1-1 reminders.
pub struct BoardDlg {
    /// Thread id (`uid` or `g_<grid>`).
    pub(crate) thread_id: String,
    /// Group vs 1-on-1 thread.
    pub(crate) is_group: bool,
    /// Active filter / compose sub-tab.
    pub(crate) tab: BoardTab,
    /// True while fetching or mutating.
    pub(crate) loading: bool,
    /// Fetched board items.
    pub(crate) items: Vec<BoardItem>,
    /// In-flight item ids to guard double clicks.
    pub(crate) busy_ids: HashSet<String>,
    /// Latest fetch or submit error.
    pub(crate) error: Option<String>,
    /// Note / reminder title input.
    pub(crate) title_input: Entity<LineInput>,
    /// Minutes from now for a new reminder (`15`, `60`, `120`, `1440`).
    pub(crate) delay_mins: i64,
    /// Repeat mode (`0` none, `1` daily, `2` weekly, `3` monthly).
    pub(crate) repeat: i64,
    /// Pin new topic note to top bar.
    pub(crate) pin_new: bool,
    /// Poll question input.
    pub(crate) poll_q: Entity<LineInput>,
    /// Poll option inputs (3 fixed rows; non-empty entries sent).
    pub(crate) poll_opts: [Entity<LineInput>; 3],
    /// Allow selecting multiple poll options.
    pub(crate) poll_multi: bool,
    /// Allow members to add options.
    pub(crate) poll_add: bool,
    /// Hide voter names.
    pub(crate) poll_anon: bool,
    /// Set while "Edit" is rewriting an existing group note or reminder.
    pub(crate) editing_id: Option<String>,
    /// Topic type of the item being edited.
    pub(crate) editing_type: i64,
    /// Original `startTime` kept when the delay chip is "Keep time" (`-1`).
    pub(crate) editing_start: i64,
    /// Pin state of the item being edited. Reminder edits do not show the pin box.
    pub(crate) editing_pinned: bool,
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|val| {
            val.as_str()
                .map(str::to_owned)
                .or_else(|| val.as_i64().map(|n| n.to_string()))
        })
        .unwrap_or_default()
}

fn parse_data_obj(v: &Value) -> Value {
    match v {
        Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// Parses `/api/board/list` or `/api/board/oneone/list` response into [`BoardItem`]s.
#[must_use]
pub fn board_items_from(value: &Value, is_group: bool) -> Vec<BoardItem> {
    let root = value.get("data").unwrap_or(value);
    let items = root
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| root.as_array());
    let Some(arr) = items else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| {
            let b_type = entry.get("boardType").and_then(Value::as_i64).unwrap_or(-1);
            let data = entry
                .get("data")
                .map(parse_data_obj)
                .unwrap_or_else(|| entry.clone());
            let topic_type = data.get("type").and_then(Value::as_i64).unwrap_or(0);
            let repeat = data.get("repeat").and_then(Value::as_i64).unwrap_or(0);
            let my_resp = data
                .get("responseMem")
                .and_then(|mem| mem.get("myResp"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if b_type == 3 || data.get("poll_id").is_some() || data.get("question").is_some() {
                let poll_id = data.get("poll_id").and_then(Value::as_i64).unwrap_or(0);
                let title = s(&data, "question");
                let opts = data
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|o| {
                                let id = o.get("option_id").and_then(Value::as_i64)?;
                                let content = s(o, "content");
                                let votes = o
                                    .get("total_vote")
                                    .and_then(Value::as_i64)
                                    .or_else(|| {
                                        o.get("voters")
                                            .and_then(Value::as_array)
                                            .map(|a| a.len() as i64)
                                    })
                                    .unwrap_or(0);
                                let voted =
                                    o.get("voted").and_then(Value::as_bool).unwrap_or(false);
                                Some(PollOption {
                                    id,
                                    content,
                                    votes,
                                    voted,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let closed = data.get("status").and_then(Value::as_i64).unwrap_or(0) != 0;
                return Some(BoardItem {
                    id: poll_id.to_string(),
                    kind: BoardKind::Poll,
                    title: if title.is_empty() {
                        "Bình chọn".into()
                    } else {
                        title
                    },
                    subtitle: if closed {
                        "Đã kết thúc".into()
                    } else {
                        "Đang mở bình chọn".into()
                    },
                    at_ms: data
                        .get("created_time")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    poll_id: Some(poll_id),
                    pin_type: 3,
                    pinned: b_type == 3,
                    options: opts,
                    closed,
                    topic_type: 0,
                    repeat: 0,
                    my_resp: 0,
                });
            }
            if !is_group || b_type == 4 || topic_type == zalo_client::TOPIC_REMINDER {
                let id = Some(s(&data, "id"))
                    .filter(|v| !v.is_empty())
                    .or_else(|| Some(s(&data, "reminderId")).filter(|v| !v.is_empty()))
                    .or_else(|| Some(s(&data, "topicId")).filter(|v| !v.is_empty()))
                    .unwrap_or_default();
                let params = data
                    .get("params")
                    .map(parse_data_obj)
                    .unwrap_or(Value::Null);
                let title = ["title", "msg", "content"]
                    .iter()
                    .find_map(|k| {
                        let v = s(&data, k);
                        (!v.is_empty()).then_some(v)
                    })
                    .or_else(|| {
                        let v = s(&params, "title");
                        (!v.is_empty()).then_some(v)
                    })
                    .unwrap_or_else(|| "Nhắc hẹn".into());
                let start = data
                    .get("startTime")
                    .or_else(|| data.get("time"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                return Some(BoardItem {
                    id,
                    kind: BoardKind::Reminder,
                    title,
                    subtitle: format_reminder_time(start),
                    at_ms: start,
                    poll_id: None,
                    pin_type: 1,
                    pinned: b_type == 1,
                    options: Vec::new(),
                    closed: false,
                    topic_type,
                    repeat,
                    my_resp,
                });
            }
            let id = Some(s(&data, "id"))
                .filter(|v| !v.is_empty())
                .or_else(|| Some(s(&data, "topicId")).filter(|v| !v.is_empty()))
                .unwrap_or_default();
            let params = data
                .get("params")
                .map(parse_data_obj)
                .unwrap_or(Value::Null);
            let title = ["title", "msg", "content"]
                .iter()
                .find_map(|k| {
                    let v = s(&data, k);
                    (!v.is_empty()).then_some(v)
                })
                .or_else(|| {
                    let v = s(&params, "title");
                    (!v.is_empty()).then_some(v)
                })
                .unwrap_or_else(|| "Tin nhắn đã ghim".into());
            let kind = if b_type == 2 {
                BoardKind::PinnedMsg
            } else {
                BoardKind::Note
            };
            Some(BoardItem {
                id,
                kind,
                title,
                subtitle: if kind == BoardKind::PinnedMsg {
                    "Tin nhắn ghim".into()
                } else {
                    "Ghi chú nhóm".into()
                },
                at_ms: data.get("createTime").and_then(Value::as_i64).unwrap_or(0),
                poll_id: None,
                pin_type: if b_type > 0 { b_type } else { 1 },
                pinned: b_type == 1 || b_type == 2,
                options: Vec::new(),
                closed: false,
                topic_type,
                repeat,
                my_resp,
            })
        })
        .collect()
}

/// Applies `getReminder` (12088) and, when present, `listResponseEvent` (12086).
///
/// Keeps the title, time, this account's RSVP, and a response count.
/// Member names from the list are not copied onto the row.
pub(crate) fn apply_reminder_detail(
    item: &mut BoardItem,
    detail: &Value,
    responses: Option<&Value>,
) {
    let root = detail.get("data").unwrap_or(detail);
    let params = root
        .get("params")
        .map(parse_data_obj)
        .unwrap_or(Value::Null);
    let title = ["title", "msg", "content"]
        .iter()
        .find_map(|k| {
            let v = s(root, k);
            (!v.is_empty()).then_some(v)
        })
        .or_else(|| {
            let v = s(&params, "title");
            (!v.is_empty()).then_some(v)
        });
    if let Some(title) = title {
        item.title = title;
    }
    if let Some(start) = root
        .get("startTime")
        .or_else(|| root.get("time"))
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
    {
        item.at_ms = start;
    }
    if let Some(my) = root
        .get("responseMem")
        .and_then(|mem| mem.get("myResp"))
        .and_then(Value::as_i64)
    {
        item.my_resp = my;
    }
    let count = responses
        .and_then(response_tally)
        .or_else(|| response_tally(root));
    let time = format_reminder_time(item.at_ms);
    item.subtitle = match count {
        Some(n) => format!("{time} · {n} phản hồi"),
        None => time,
    };
}

/// Accept + reject counts. Arrays are counted by length. Numbers are counts.
fn response_tally(value: &Value) -> Option<usize> {
    let root = value.get("data").unwrap_or(value);
    if let Some(n) = pair_count(root) {
        return Some(n);
    }
    if let Some(mem) = root.get("responseMem") {
        if let Some(n) = pair_count(mem) {
            return Some(n);
        }
    }
    for key in ["responses", "list", "items", "responseMems"] {
        if let Some(items) = root.get(key).and_then(Value::as_array) {
            return Some(items.len());
        }
    }
    None
}

fn pair_count(value: &Value) -> Option<usize> {
    let accept = member_count(value, "acceptMember");
    let reject = member_count(value, "rejectMember");
    match (accept, reject) {
        (None, None) => None,
        (accept, reject) => Some(accept.unwrap_or(0) + reject.unwrap_or(0)),
    }
}

fn member_count(value: &Value, key: &str) -> Option<usize> {
    match value.get(key)? {
        Value::Array(items) => Some(items.len()),
        Value::Number(n) => Some(n.as_u64().unwrap_or(0) as usize),
        Value::String(raw) => raw.parse().ok(),
        _ => None,
    }
}

fn format_reminder_time(ts: i64) -> String {
    if ts <= 0 {
        return "Nhắc hẹn".into();
    }
    let ms = if ts < 10_000_000_000 { ts * 1000 } else { ts };
    let (y, m, d) = crate::data::vn_ymd(ms);
    let secs = (ms / 1000) + 7 * 3600;
    let hh = (secs / 3600).rem_euclid(24);
    let mm = (secs / 60).rem_euclid(60);
    format!("{d:02}/{m:02}/{y} {hh:02}:{mm:02}")
}

impl ShellView {
    /// Open Group Board (notes/pins/polls) or 1-1 Reminder modal cleanly.
    pub(crate) fn open_board_dialog(
        &mut self,
        conv_id: u64,
        tab: BoardTab,
        cx: &mut Context<Self>,
    ) {
        let Some(conv) = self.convs.iter().find(|c| c.id == conv_id).cloned() else {
            return;
        };
        let title_input = cx.new(|cx| LineInput::new(cx, "Nhập nội dung…"));
        let poll_q = cx.new(|cx| LineInput::new(cx, "Đặt câu hỏi bình chọn…"));
        let poll_opts = [
            cx.new(|cx| LineInput::new(cx, "Phương án 1")),
            cx.new(|cx| LineInput::new(cx, "Phương án 2")),
            cx.new(|cx| LineInput::new(cx, "Phương án 3 (tuỳ chọn)")),
        ];
        let dlg = BoardDlg {
            thread_id: conv.thread_id.clone(),
            is_group: conv.is_group,
            tab,
            loading: self.has_session,
            items: Vec::new(),
            busy_ids: HashSet::new(),
            error: None,
            title_input,
            delay_mins: 60,
            repeat: 0,
            pin_new: true,
            poll_q,
            poll_opts,
            poll_multi: false,
            poll_add: true,
            poll_anon: false,
            editing_id: None,
            editing_type: 0,
            editing_start: 0,
            editing_pinned: false,
        };
        self.dialog = Some(crate::dialogs::Dialog::Board(dlg));
        self.info_open = true;
        self.refresh_board_dialog(cx);
    }

    /// Open "Create Reminder" pre-populated from a message's text (message context menu).
    pub(crate) fn open_board_reminder_with_text(
        &mut self,
        conv_id: u64,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        self.open_board_dialog(conv_id, BoardTab::CreateReminder, cx);
        let snippet: String = text.trim().chars().take(120).collect();
        if !snippet.is_empty() {
            if let Some(crate::dialogs::Dialog::Board(b)) = &self.dialog {
                b.title_input.update(cx, |i, cx| i.set_text(snippet, cx));
            }
        }
    }

    pub(crate) fn refresh_board_dialog(&mut self, cx: &mut Context<Self>) {
        let (thread_id, is_group) = match &mut self.dialog {
            Some(crate::dialogs::Dialog::Board(d)) => {
                if !self.has_session {
                    d.loading = false;
                    return;
                }
                d.loading = true;
                d.error = None;
                (d.thread_id.clone(), d.is_group)
            }
            _ => return,
        };
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let tid = thread_id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let raw = if is_group {
                            api.list_board(&tid, zalo_client::BOARD_ALL, 0, 50)
                                .await
                                .map_err(|e| e.to_string())?
                        } else {
                            api.list_oneone_board(&tid, zalo_client::BOARD_ALL, 0, 50)
                                .await
                                .map_err(|e| e.to_string())?
                        };
                        Ok(board_items_from(&raw, is_group))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(crate::dialogs::Dialog::Board(d)) = &mut view.dialog {
                    if d.thread_id == thread_id {
                        d.loading = false;
                        match result {
                            Ok(items) => d.items = items,
                            Err(e) => {
                                // Code 114 / empty board on fresh groups: show clean empty state
                                // instead of raw API error text.
                                if e.contains("returned 114") {
                                    d.error = None;
                                } else {
                                    d.error = Some(
                                        "Không thể tải bảng tin lúc này. Vui lòng thử lại sau."
                                            .to_owned(),
                                    );
                                }
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn submit_board_form(&mut self, cx: &mut Context<Self>) {
        let Some(crate::dialogs::Dialog::Board(d)) = &mut self.dialog else {
            return;
        };
        let tab = d.tab;
        let thread_id = d.thread_id.clone();
        let is_group = d.is_group;
        let me_uid = self.uin.clone().unwrap_or_default();
        let now_s = crate::data::now_ms() / 1000;
        let title = d.title_input.read(cx).text().trim().to_owned();
        let delay = d.delay_mins;
        let repeat = d.repeat;
        let pin_new = d.pin_new;
        let poll_q = d.poll_q.read(cx).text().trim().to_owned();
        let poll_opts: Vec<String> = d
            .poll_opts
            .iter()
            .map(|e| e.read(cx).text().trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        let multi = d.poll_multi;
        let add = d.poll_add;
        let anon = d.poll_anon;
        let editing_id = d.editing_id.clone();
        let editing_type = d.editing_type;
        let editing_start = d.editing_start;
        let editing_pinned = d.editing_pinned;

        match tab {
            BoardTab::CreateReminder | BoardTab::CreateNote if title.is_empty() => {
                d.error = Some("Vui lòng nhập nội dung".into());
                return;
            }
            BoardTab::CreatePoll if poll_q.is_empty() || poll_opts.len() < 2 => {
                d.error = Some("Cần câu hỏi và ít nhất 2 phương án".into());
                return;
            }
            _ => {}
        }

        if !self.has_session {
            if let Some(id) = editing_id.clone() {
                if let Some(item) = d.items.iter_mut().find(|it| it.id == id) {
                    if matches!(tab, BoardTab::CreateNote | BoardTab::CreateReminder) {
                        item.title.clone_from(&title);
                        if tab == BoardTab::CreateNote {
                            item.pinned = pin_new;
                        } else if delay >= 0 {
                            let at = (now_s + delay * 60) * 1000;
                            item.at_ms = at;
                            item.subtitle = format_reminder_time(at);
                            item.repeat = repeat;
                        }
                    }
                }
                d.editing_id = None;
                d.tab = BoardTab::All;
                d.title_input.update(cx, |i, cx| i.clear(cx));
                return;
            }
            // Preview / offline: insert local preview item cleanly.
            let new_item = match tab {
                BoardTab::CreatePoll => BoardItem {
                    id: format!("local-poll-{now_s}"),
                    kind: BoardKind::Poll,
                    title: poll_q,
                    subtitle: "Bản xem trước".into(),
                    at_ms: crate::data::now_ms(),
                    poll_id: None,
                    pin_type: 3,
                    pinned: false,
                    options: poll_opts
                        .into_iter()
                        .enumerate()
                        .map(|(i, c)| PollOption {
                            id: i as i64 + 1,
                            content: c,
                            votes: 0,
                            voted: false,
                        })
                        .collect(),
                    closed: false,
                    topic_type: 0,
                    repeat: 0,
                    my_resp: 0,
                },
                BoardTab::CreateNote => BoardItem {
                    id: format!("local-note-{now_s}"),
                    kind: BoardKind::Note,
                    title,
                    subtitle: "Ghi chú (preview)".into(),
                    at_ms: crate::data::now_ms(),
                    poll_id: None,
                    pin_type: 1,
                    pinned: pin_new,
                    options: Vec::new(),
                    closed: false,
                    topic_type: zalo_client::TOPIC_TEXT,
                    repeat: 0,
                    my_resp: 0,
                },
                _ => BoardItem {
                    id: format!("local-rem-{now_s}"),
                    kind: BoardKind::Reminder,
                    title,
                    subtitle: format_reminder_time((now_s + delay * 60) * 1000),
                    at_ms: (now_s + delay * 60) * 1000,
                    poll_id: None,
                    pin_type: 1,
                    pinned: false,
                    options: Vec::new(),
                    closed: false,
                    topic_type: zalo_client::TOPIC_REMINDER,
                    repeat,
                    my_resp: 0,
                },
            };
            d.items.insert(0, new_item);
            d.tab = BoardTab::All;
            d.title_input.update(cx, |i, cx| i.clear(cx));
            d.poll_q.update(cx, |i, cx| i.clear(cx));
            return;
        }

        d.loading = true;
        d.error = None;
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let start_s = now_s + delay * 60;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        match tab {
                            BoardTab::CreateReminder => {
                                if is_group {
                                    if let Some(topic_id) = editing_id.clone() {
                                        let start = if delay < 0 {
                                            if editing_start > 0 {
                                                editing_start
                                            } else {
                                                -1
                                            }
                                        } else {
                                            start_s
                                        };
                                        let pin_act = if editing_pinned { 1 } else { 2 };
                                        let kind = if editing_type == 0 {
                                            zalo_client::TOPIC_REMINDER
                                        } else {
                                            editing_type
                                        };
                                        api.update_group_topic(
                                            &thread_id, &topic_id, kind, &title, start, repeat,
                                            pin_act,
                                        )
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                    } else {
                                        api.create_group_topic(
                                            &thread_id,
                                            zalo_client::TOPIC_REMINDER,
                                            &title,
                                            start_s,
                                            repeat,
                                            1,
                                        )
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                    }
                                } else {
                                    api.create_oneone_reminder(
                                        &thread_id,
                                        &title,
                                        start_s * 1000,
                                        repeat,
                                        &me_uid,
                                    )
                                    .await
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                                }
                            }
                            BoardTab::CreateNote => {
                                if let Some(topic_id) = editing_id.clone() {
                                    api.update_group_topic(
                                        &thread_id,
                                        &topic_id,
                                        zalo_client::TOPIC_TEXT,
                                        &title,
                                        -1,
                                        0,
                                        if pin_new { 1 } else { 2 },
                                    )
                                    .await
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                                } else {
                                    api.create_group_topic(
                                        &thread_id,
                                        zalo_client::TOPIC_TEXT,
                                        &title,
                                        0,
                                        0,
                                        if pin_new { 1 } else { 0 },
                                    )
                                    .await
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                                }
                            }
                            BoardTab::CreatePoll => api
                                .create_poll(&thread_id, &poll_q, &poll_opts, multi, add, anon)
                                .await
                                .map(|_| ())
                                .map_err(|e| e.to_string()),
                            _ => Ok(()),
                        }
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(crate::dialogs::Dialog::Board(d)) = &mut view.dialog {
                    d.loading = false;
                    match result {
                        Ok(()) => {
                            d.editing_id = None;
                            d.tab = BoardTab::All;
                            d.title_input.update(cx, |i, cx| i.clear(cx));
                            d.poll_q.update(cx, |i, cx| i.clear(cx));
                            for opt in &d.poll_opts {
                                opt.update(cx, |i, cx| i.clear(cx));
                            }
                            view.set_note("Đã lưu lên bảng tin");
                            view.refresh_board_dialog(cx);
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn remove_board_entry(&mut self, item: &BoardItem, cx: &mut Context<Self>) {
        let Some(crate::dialogs::Dialog::Board(d)) = &mut self.dialog else {
            return;
        };
        let thread_id = d.thread_id.clone();
        let is_group = d.is_group;
        let id = item.id.clone();
        let kind = item.kind;
        if !self.has_session {
            d.items.retain(|it| it.id != id);
            return;
        }
        d.busy_ids.insert(id.clone());
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let item_id = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        if !is_group && kind == BoardKind::Reminder {
                            api.remove_oneone_reminder(&thread_id, &item_id)
                                .await
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        } else if kind == BoardKind::Poll {
                            if let Ok(pid) = item_id.parse::<i64>() {
                                api.end_poll(pid)
                                    .await
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                            } else {
                                Ok(())
                            }
                        } else {
                            api.remove_group_topic(&thread_id, &item_id)
                                .await
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        }
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(crate::dialogs::Dialog::Board(d)) = &mut view.dialog {
                    d.busy_ids.remove(&id);
                    match result {
                        Ok(()) => view.refresh_board_dialog(cx),
                        Err(e) => d.error = Some(e),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the create form prefilled so "Save" calls `updatev2` instead of create.
    pub(crate) fn begin_board_edit(&mut self, item: &BoardItem, cx: &mut Context<Self>) {
        let Some(crate::dialogs::Dialog::Board(b)) = &mut self.dialog else {
            return;
        };
        if !b.is_group || item.id.is_empty() {
            return;
        }
        b.editing_id = Some(item.id.clone());
        b.editing_type = if item.kind == BoardKind::Reminder {
            zalo_client::TOPIC_REMINDER
        } else {
            item.topic_type
        };
        b.editing_start = item.at_ms;
        b.editing_pinned = item.pinned;
        b.repeat = item.repeat;
        b.delay_mins = -1;
        b.pin_new = item.pinned;
        b.error = None;
        b.tab = if item.kind == BoardKind::Reminder {
            BoardTab::CreateReminder
        } else {
            BoardTab::CreateNote
        };
        let title = item.title.clone();
        b.title_input
            .update(cx, |input, cx| input.set_text(title, cx));
    }

    /// `EVENT_STATUS` accept (`1`) or reject (`2`) for a group reminder.
    pub(crate) fn respond_board_event(
        &mut self,
        item_id: &str,
        status: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(crate::dialogs::Dialog::Board(d)) = &mut self.dialog else {
            return;
        };
        if !d.is_group || item_id.is_empty() {
            return;
        }
        let thread_id = d.thread_id.clone();
        let id = item_id.to_owned();
        if !self.has_session {
            if let Some(item) = d.items.iter_mut().find(|it| it.id == id) {
                item.my_resp = status;
            }
            return;
        }
        d.busy_ids.insert(id.clone());
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let item_id = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.respond_group_event(&thread_id, status, &item_id)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(crate::dialogs::Dialog::Board(d)) = &mut view.dialog {
                    d.busy_ids.remove(&id);
                    match result {
                        Ok(()) => {
                            if let Some(item) = d.items.iter_mut().find(|it| it.id == id) {
                                item.my_resp = status;
                            }
                            view.set_note(if status == zalo_client::EVENT_REJECT {
                                "Đã từ chối nhắc hẹn".to_owned()
                            } else {
                                "Đã nhận lời nhắc".to_owned()
                            });
                            view.refresh_board_dialog(cx);
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Loads reminder detail (12088) and the response list (12086) for one row.
    ///
    /// The title click is the open action. RSVP chips stay as they were; this
    /// only fills the subtitle with the time and a response count.
    pub(crate) fn load_reminder_detail(&mut self, item_id: &str, cx: &mut Context<Self>) {
        let Some(crate::dialogs::Dialog::Board(d)) = &mut self.dialog else {
            return;
        };
        if !d.is_group || item_id.is_empty() || d.busy_ids.contains(item_id) {
            return;
        }
        if !self.has_session {
            self.set_note("Chưa đăng nhập");
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            self.set_note("Chưa đăng nhập");
            return;
        };
        let id = item_id.to_owned();
        d.busy_ids.insert(id.clone());
        self.set_note("Đang tải chi tiết nhắc hẹn");
        cx.spawn(async move |this, cx| {
            let item_id = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let detail = api
                            .get_reminder_detail(&item_id)
                            .await
                            .map_err(|e| e.to_string())?;
                        let responses = api.list_event_responses(&item_id).await.ok();
                        Ok::<_, String>((detail, responses))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(crate::dialogs::Dialog::Board(d)) = &mut view.dialog {
                    d.busy_ids.remove(&id);
                    match result {
                        Ok((detail, responses)) => {
                            if let Some(item) = d.items.iter_mut().find(|it| it.id == id) {
                                apply_reminder_detail(item, &detail, responses.as_ref());
                            }
                            view.set_note("Đã tải chi tiết nhắc hẹn");
                        }
                        Err(err) => d.error = Some(err),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn board_modal(
        &self,
        d: &BoardDlg,
        corners: crate::window_frame::CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let red = token_rgb("--R60", "#c31818");
        let title = if d.is_group {
            "Bảng tin nhóm"
        } else {
            "Danh sách nhắc hẹn"
        };
        let view_tabs: Vec<(BoardTab, &'static str)> = if d.is_group {
            vec![
                (BoardTab::All, "Tất cả"),
                (BoardTab::Notes, "Ghi chú"),
                (BoardTab::Polls, "Bình chọn"),
                (BoardTab::Reminders, "Nhắc hẹn"),
            ]
        } else {
            vec![(BoardTab::All, "Nhắc hẹn")]
        };
        let create_tabs: Vec<(BoardTab, &'static str)> = if d.is_group {
            vec![
                (BoardTab::CreateNote, "+ Ghi chú"),
                (BoardTab::CreatePoll, "+ Bình chọn"),
                (BoardTab::CreateReminder, "+ Nhắc hẹn"),
            ]
        } else {
            vec![(BoardTab::CreateReminder, "+ Tạo nhắc hẹn")]
        };
        let tab_bar = div()
            .flex()
            .flex_col()
            .flex_none()
            .border_b_1()
            .border_color(border_subtle())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(38.))
                    .px(px(12.))
                    .gap(px(12.))
                    .border_b_1()
                    .border_color(border_subtle())
                    .children(view_tabs.into_iter().enumerate().map(|(i, (tab, label))| {
                        let on = d.tab == tab;
                        div()
                            .id(("board-tab", i as u64))
                            .flex()
                            .items_center()
                            .h_full()
                            .mb(px(-1.))
                            .cursor_pointer()
                            .text_size(px(13.))
                            .font_family(FAMILY_SEMIBOLD)
                            .border_b_2()
                            .border_color(if on {
                                rgb(blue).into()
                            } else {
                                gpui::hsla(0., 0., 0., 0.)
                            })
                            .text_color(rgb(if on { blue } else { muted }))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(crate::dialogs::Dialog::Board(b)) = &mut this.dialog {
                                    b.tab = tab;
                                    b.error = None;
                                    b.editing_id = None;
                                    if b.delay_mins < 0 {
                                        b.delay_mins = 60;
                                    }
                                }
                                cx.notify();
                            }))
                            .child(label)
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .px(px(12.))
                    .py(px(8.))
                    .children(create_tabs.into_iter().enumerate().map(|(i, (tab, label))| {
                        let on = d.tab == tab;
                        div()
                            .id(("board-create-tab", i as u64))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(12.))
                            .cursor_pointer()
                            .text_size(px(12.))
                            .font_family(FAMILY_SEMIBOLD)
                            .bg(rgb(if on { blue } else { subtle }))
                            .text_color(rgb(if on { 0xffffff } else { ink }))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(crate::dialogs::Dialog::Board(b)) = &mut this.dialog {
                                    b.tab = tab;
                                    b.error = None;
                                    b.editing_id = None;
                                    if b.delay_mins < 0 {
                                        b.delay_mins = 60;
                                    }
                                }
                                cx.notify();
                            }))
                            .child(label)
                    })),
            );

        let creating = matches!(
            d.tab,
            BoardTab::CreateReminder | BoardTab::CreateNote | BoardTab::CreatePoll
        );

        let body: gpui::AnyElement = match d.tab {
            BoardTab::CreateReminder | BoardTab::CreateNote => {
                let is_rem = d.tab == BoardTab::CreateReminder;
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .p(px(16.))
                    .when(d.editing_id.is_some(), |el| {
                        el.child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(blue))
                                .child("Đang sửa mục đã có"),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child(if is_rem {
                                "Nội dung nhắc hẹn"
                            } else {
                                "Nội dung ghi chú nhóm"
                            }),
                    )
                    .child(
                        div()
                            .px(px(12.))
                            .py(px(8.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(border_subtle())
                            .child(d.title_input.clone()),
                    )
                    .when(is_rem, |el| {
                        el.child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(muted))
                                .child("Thời gian nhắc"),
                        )
                        .child(
                            div().flex().flex_row().flex_wrap().gap(px(6.)).children(
                                {
                                    let mut delays = vec![
                                        (15_i64, "15 phút"),
                                        (60, "1 giờ"),
                                        (120, "2 giờ"),
                                        (1440, "Ngày mai"),
                                    ];
                                    if d.editing_id.is_some() {
                                        delays.insert(0, (-1, "Giữ giờ"));
                                    }
                                    delays
                                }
                                .into_iter()
                                .enumerate()
                                .map(|(i, (mins, lbl))| {
                                    let on = d.delay_mins == mins;
                                    div()
                                        .id(("rem-delay", i as u64))
                                        .px(px(10.))
                                        .py(px(5.))
                                        .rounded(px(6.))
                                        .cursor_pointer()
                                        .bg(rgb(if on { blue } else { subtle }))
                                        .text_color(rgb(if on { 0xffffff } else { ink }))
                                        .text_size(px(12.5))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(crate::dialogs::Dialog::Board(b)) =
                                                &mut this.dialog
                                            {
                                                b.delay_mins = mins;
                                            }
                                            cx.notify();
                                        }))
                                        .child(lbl)
                                }),
                            ),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(muted))
                                .child("Lặp lại"),
                        )
                        .child(
                            div().flex().flex_row().flex_wrap().gap(px(6.)).children(
                                [
                                    (0_i64, "Không lặp"),
                                    (1, "Hàng ngày"),
                                    (2, "Hàng tuần"),
                                    (3, "Hàng tháng"),
                                ]
                                .into_iter()
                                .enumerate()
                                .map(|(i, (rep, lbl))| {
                                    let on = d.repeat == rep;
                                    div()
                                        .id(("rem-rep", i as u64))
                                        .px(px(10.))
                                        .py(px(5.))
                                        .rounded(px(6.))
                                        .cursor_pointer()
                                        .bg(rgb(if on { blue } else { subtle }))
                                        .text_color(rgb(if on { 0xffffff } else { ink }))
                                        .text_size(px(12.5))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(crate::dialogs::Dialog::Board(b)) =
                                                &mut this.dialog
                                            {
                                                b.repeat = rep;
                                            }
                                            cx.notify();
                                        }))
                                        .child(lbl)
                                }),
                            ),
                        )
                    })
                    .when(!is_rem, |el| {
                        el.child(
                            div()
                                .id("note-pin-toggle")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .cursor_pointer()
                                .text_size(px(13.5))
                                .text_color(rgb(ink))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(crate::dialogs::Dialog::Board(b)) = &mut this.dialog
                                    {
                                        b.pin_new = !b.pin_new;
                                    }
                                    cx.notify();
                                }))
                                .child(if d.pin_new {
                                    "☑ Ghim lên đầu hội thoại"
                                } else {
                                    "☐ Ghim lên đầu hội thoại"
                                }),
                        )
                    })
                    .into_any_element()
            }
            BoardTab::CreatePoll => div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(16.))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .child("Câu hỏi bình chọn"),
                )
                .child(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(border_subtle())
                        .child(d.poll_q.clone()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .child("Các lựa chọn"),
                )
                .children(d.poll_opts.iter().map(|opt| {
                    div()
                        .px(px(12.))
                        .py(px(7.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(border_subtle())
                        .child(opt.clone())
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(12.))
                        .pt(px(4.))
                        .child(
                            div()
                                .id("poll-multi")
                                .cursor_pointer()
                                .text_size(px(13.))
                                .text_color(rgb(ink))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(crate::dialogs::Dialog::Board(b)) = &mut this.dialog
                                    {
                                        b.poll_multi = !b.poll_multi;
                                    }
                                    cx.notify();
                                }))
                                .child(if d.poll_multi {
                                    "☑ Chọn nhiều"
                                } else {
                                    "☐ Chọn nhiều"
                                }),
                        )
                        .child(
                            div()
                                .id("poll-add")
                                .cursor_pointer()
                                .text_size(px(13.))
                                .text_color(rgb(ink))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(crate::dialogs::Dialog::Board(b)) = &mut this.dialog
                                    {
                                        b.poll_add = !b.poll_add;
                                    }
                                    cx.notify();
                                }))
                                .child(if d.poll_add {
                                    "☑ Thành viên thêm chọn"
                                } else {
                                    "☐ Thành viên thêm chọn"
                                }),
                        )
                        .child(
                            div()
                                .id("poll-anon")
                                .cursor_pointer()
                                .text_size(px(13.))
                                .text_color(rgb(ink))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(crate::dialogs::Dialog::Board(b)) = &mut this.dialog
                                    {
                                        b.poll_anon = !b.poll_anon;
                                    }
                                    cx.notify();
                                }))
                                .child(if d.poll_anon {
                                    "☑ Ẩn danh"
                                } else {
                                    "☐ Ẩn danh"
                                }),
                        ),
                )
                .into_any_element(),
            _ => {
                let filtered: Vec<&BoardItem> = d
                    .items
                    .iter()
                    .filter(|it| match d.tab {
                        BoardTab::Reminders => it.kind == BoardKind::Reminder,
                        BoardTab::Notes => {
                            matches!(it.kind, BoardKind::Note | BoardKind::PinnedMsg)
                        }
                        BoardTab::Polls => it.kind == BoardKind::Poll,
                        _ => true,
                    })
                    .collect();
                div()
                    .id("board-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(px(16.))
                    .gap(px(10.))
                    .when(d.loading, |el| {
                        el.child(div().text_size(px(13.5)).text_color(rgb(muted)).child("Đang tải bảng tin…"))
                    })
                    .when(!d.loading && filtered.is_empty(), |el| {
                        el.child(
                            div()
                                .py(px(28.))
                                .text_center()
                                .text_size(px(14.))
                                .text_color(rgb(muted))
                                .child("Chưa có mục nào trên bảng tin"),
                        )
                    })
                    .children(filtered.into_iter().enumerate().map(|(i, item)| {
                        let rm_item = item.clone();
                        let pin_tid = d.thread_id.clone();
                        let pin_id = item.id.clone();
                        let pin_type = item.pin_type;
                        let is_pinned = item.pinned;
                        let is_group = d.is_group;
                        let can_pin = is_group && item.kind != BoardKind::Poll && !pin_id.is_empty();
                        let edit_item = item.clone();
                        let rsvp_id = item.id.clone();
                        let my_resp = item.my_resp;
                        let can_edit = is_group
                            && matches!(item.kind, BoardKind::Note | BoardKind::Reminder)
                            && !pin_id.is_empty();
                        let can_rsvp = is_group && item.kind == BoardKind::Reminder && !pin_id.is_empty();
                        div()
                            .id(("board-item", i as u64))
                            .p(px(12.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(border_subtle())
                            .bg(rgb(subtle))
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_start()
                                    .justify_between()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.))
                                            .child({
                                                let open_id = rsvp_id.clone();
                                                div()
                                                    .id(("board-open", i as u64))
                                                    .text_size(px(14.))
                                                    .font_family(FAMILY_SEMIBOLD)
                                                    .text_color(rgb(ink))
                                                    .when(can_rsvp, |el| {
                                                        el.cursor_pointer().on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.load_reminder_detail(&open_id, cx);
                                                                cx.notify();
                                                            },
                                                        ))
                                                    })
                                                    .child(item.title.clone())
                                            })
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(muted))
                                                    .child(item.subtitle.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .gap(px(6.))
                                            .when(can_pin, |el| {
                                                el.child(
                                                    div()
                                                        .id(("board-pin", i as u64))
                                                        .px(px(8.))
                                                        .py(px(3.))
                                                        .rounded(px(4.))
                                                        .bg(rgb(paper))
                                                        .hover(|s| s.bg(rgb(hover)))
                                                        .cursor_pointer()
                                                        .text_size(px(11.5))
                                                        .text_color(rgb(blue))
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            if let Some(dir) = this.session_dir.clone() {
                                                                let act = if is_pinned {
                                                                    crate::bridge::LiveAction::UnpinBoard {
                                                                        thread_id: pin_tid.clone(),
                                                                        topic_id: pin_id.clone(),
                                                                        board_version: 0,
                                                                    }
                                                                } else {
                                                                    crate::bridge::LiveAction::PinBoard {
                                                                        thread_id: pin_tid.clone(),
                                                                        topic_id: pin_id.clone(),
                                                                        item_type: pin_type,
                                                                    }
                                                                };
                                                                crate::bridge::spawn_live(act, dir);
                                                            }
                                                            this.refresh_board_dialog(cx);
                                                            cx.notify();
                                                        }))
                                                        .child(if is_pinned { "Bỏ ghim" } else { "Ghim" }),
                                                )
                                            })
                                            .when(can_rsvp, |el| {
                                                let yes_id = rsvp_id.clone();
                                                let no_id = rsvp_id.clone();
                                                let chip = |id_suffix: u64, label: &'static str, on: bool, event_id: String, status: i64| {
                                                    div()
                                                        .id(("board-rsvp", i as u64 * 2 + id_suffix))
                                                        .px(px(8.))
                                                        .py(px(3.))
                                                        .rounded(px(4.))
                                                        .bg(rgb(if on { blue } else { paper }))
                                                        .hover(|s| s.bg(rgb(if on { blue } else { hover })))
                                                        .cursor_pointer()
                                                        .text_size(px(11.5))
                                                        .text_color(rgb(if on { 0xffffff } else { blue }))
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            this.respond_board_event(&event_id, status, cx);
                                                            cx.notify();
                                                        }))
                                                        .child(label)
                                                };
                                                el.child(chip(0, "Tham gia", my_resp == zalo_client::EVENT_ACCEPT, yes_id, zalo_client::EVENT_ACCEPT))
                                                    .child(chip(1, "Từ chối", my_resp == zalo_client::EVENT_REJECT, no_id, zalo_client::EVENT_REJECT))
                                            })
                                            .when(can_edit, |el| {
                                                el.child(
                                                    div()
                                                        .id(("board-edit", i as u64))
                                                        .px(px(8.))
                                                        .py(px(3.))
                                                        .rounded(px(4.))
                                                        .bg(rgb(paper))
                                                        .hover(|s| s.bg(rgb(hover)))
                                                        .cursor_pointer()
                                                        .text_size(px(11.5))
                                                        .text_color(rgb(blue))
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            this.begin_board_edit(&edit_item, cx);
                                                            cx.notify();
                                                        }))
                                                        .child("Sửa"),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .id(("board-del", i as u64))
                                                    .px(px(8.))
                                                    .py(px(3.))
                                                    .rounded(px(4.))
                                                    .bg(rgb(paper))
                                                    .hover(|s| s.bg(rgb(hover)))
                                                    .cursor_pointer()
                                                    .text_size(px(11.5))
                                                    .text_color(rgb(red))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.remove_board_entry(&rm_item, cx);
                                                        cx.notify();
                                                    }))
                                                    .child(if item.kind == BoardKind::Poll { "Đóng" } else { "Xóa" }),
                                            ),
                                    ),
                            )
                            .when(!item.options.is_empty(), |el| {
                                let pid = item.poll_id;
                                let closed = item.closed;
                                el.children(item.options.iter().enumerate().map(|(oi, opt)| {
                                    let oid = opt.id;
                                    let voted = opt.voted;
                                    div()
                                        .id(("poll-opt", (i * 32 + oi) as u64))
                                        .px(px(10.))
                                        .py(px(6.))
                                        .rounded(px(6.))
                                        .bg(rgb(paper))
                                        .border_1()
                                        .border_color(if voted { rgb(blue).into() } else { border_subtle() })
                                        .flex()
                                        .flex_row()
                                        .justify_between()
                                        .when(!closed && pid.is_some(), |el| {
                                            el.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                                                if let (Some(poll_id), Some(dir)) = (pid, this.session_dir.clone()) {
                                                    crate::bridge::spawn_live(
                                                        crate::bridge::LiveAction::VotePoll {
                                                            poll_id,
                                                            option_ids: vec![oid],
                                                        },
                                                        dir,
                                                    );
                                                    this.set_note("Đã gửi bình chọn");
                                                    this.refresh_board_dialog(cx);
                                                }
                                                cx.notify();
                                            }))
                                        })
                                        .child(div().text_size(px(13.)).text_color(rgb(ink)).child(opt.content.clone()))
                                        .child(div().text_size(px(12.)).text_color(rgb(muted)).child(format!("{} phiếu", opt.votes)))
                                }))
                            })
                    }))
                    .into_any_element()
            }
        };

        corners
            .apply(
                div()
                    .id("conversation-info")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w(px(crate::info::INFO_PANEL_W))
                    .h_full()
                    .bg(rgb(paper))
                    .border_l_1()
                    .border_color(border_subtle())
                    .text_color(rgb(ink)),
                false,
                false,
                false,
                true,
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .h(px(zalo_design::CHAT_HEADER_H))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(border_subtle())
                    .child(
                        div()
                            .id("board-back")
                            .size(px(32.))
                            .rounded(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |st| st.bg(rgb(hover)))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.dialog = None;
                                cx.notify();
                            }))
                            .child(crate::fonts::icon_el('\u{EA8E}', "‹", 20.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_center()
                            .pr(px(32.))
                            .text_size(px(16.))
                            .font_family(FAMILY_SEMIBOLD)
                            .child(title),
                    ),
            )
            .child(tab_bar)
            .when_some(d.error.clone(), |el, err| {
                el.child(
                    div()
                        .px(px(16.))
                        .pt(px(8.))
                        .text_size(px(12.5))
                        .text_color(rgb(red))
                        .child(err),
                )
            })
            .child(body)
            .when(creating, |el| {
                el.child(dialog_footer(
                    str_vi("STR_CANCEL", "Hủy"),
                    if d.editing_id.is_some() {
                        "Lưu"
                    } else {
                        str_vi("STR_CONFIRM", "Tạo ngay")
                    },
                    !d.loading,
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(crate::dialogs::Dialog::Board(b)) = &mut this.dialog {
                            b.tab = BoardTab::All;
                            b.error = None;
                            b.editing_id = None;
                        }
                        cx.notify();
                    }),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.submit_board_form(cx);
                        cx.notify();
                    }),
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_board_items_across_group_and_oneone() {
        let v = serde_json::json!({
            "data": {
                "items": [
                    {
                        "boardType": 3,
                        "data": serde_json::json!({
                            "poll_id": 77,
                            "question": "Ăn trưa gì?",
                            "status": 0,
                            "options": [
                                {"option_id": 1, "content": "Phở", "total_vote": 3, "voted": true},
                                {"option_id": 2, "content": "Bún chả", "total_vote": 1, "voted": false}
                            ]
                        }).to_string()
                    },
                    {
                        "boardType": 1,
                        "data": {
                            "id": "t9",
                            "type": 4,
                            "title": "Họp 14:00",
                            "startTime": 1_700_000_000,
                            "repeat": 2,
                            "responseMem": { "myResp": 1 }
                        }
                    },
                    {
                        "boardType": 2,
                        "data": { "id": "m5", "msg": "Link tài liệu" }
                    }
                ]
            }
        });
        let items = board_items_from(&v, true);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].kind, BoardKind::Poll);
        assert_eq!(items[0].options.len(), 2);
        assert_eq!(items[0].options[0].votes, 3);
        assert_eq!(items[1].kind, BoardKind::Reminder);
        assert_eq!(items[1].title, "Họp 14:00");
        assert_eq!(items[1].topic_type, zalo_client::TOPIC_REMINDER);
        assert_eq!(items[1].repeat, 2);
        assert_eq!(items[1].my_resp, zalo_client::EVENT_ACCEPT);
        assert_eq!(items[2].kind, BoardKind::PinnedMsg);
    }

    #[test]
    fn reminder_detail_keeps_count_not_member_names() {
        let mut item = BoardItem {
            id: "88".into(),
            kind: BoardKind::Reminder,
            title: "cũ".into(),
            subtitle: String::new(),
            at_ms: 0,
            poll_id: None,
            pin_type: 1,
            pinned: false,
            options: Vec::new(),
            closed: false,
            topic_type: zalo_client::TOPIC_REMINDER,
            repeat: 0,
            my_resp: 0,
        };
        let detail = serde_json::json!({
            "id": "88",
            "startTime": 1_700_000_000_000_i64,
            "params": { "title": "Họp nhóm" },
            "responseMem": { "myResp": 1, "acceptMember": 2, "rejectMember": 1 }
        });
        let responses = serde_json::json!({
            "acceptMember": [{"id": "1"}, {"id": "2"}],
            "rejectMember": [{"id": "3"}]
        });
        apply_reminder_detail(&mut item, &detail, Some(&responses));
        assert_eq!(item.title, "Họp nhóm");
        assert_eq!(item.my_resp, zalo_client::EVENT_ACCEPT);
        assert_eq!(item.at_ms, 1_700_000_000_000);
        assert!(item.subtitle.contains("3 phản hồi"));
        assert!(!item.subtitle.contains("\"id\""));
    }
}
