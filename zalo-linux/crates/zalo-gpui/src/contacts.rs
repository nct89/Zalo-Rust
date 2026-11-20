//! Contacts pages, after Zalo PC: "Friends (n)" / "Groups & Communities (n)" in
//! a white card with a search field and sort toggle (friends grouped A–Z),
//! and "Friend Requests" with received (Accept / Decline) and sent
//! (Revoke request) cards.
//!
//! Data: friends and groups come from the profile cache (`get_friends`,
//! `get_group_list` + `getmg-v2`); requests from
//! `/api/friend/recommendsv2/list` (`recommType` 2 = received) and
//! `/api/friend/requested/list`.

use std::collections::HashSet;

use gpui::{
    div, prelude::*, px, rgb, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};
use serde_json::Value;

use crate::color::token_rgb;
use crate::dialogs::Person;
use crate::fonts::{self, FAMILY_SEMIBOLD};
use crate::shell::ShellView;

/// One friend request card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// Person.
    pub person: Person,
    /// Greeting text.
    pub message: String,
    /// Unix ms (0 when unknown).
    pub at_ms: i64,
}

/// "Friend Requests" page state.
#[derive(Clone, Debug, Default)]
pub struct Requests {
    /// Still fetching.
    pub loading: bool,
    /// Received (`recommType` 2).
    pub received: Vec<Request>,
    /// Sent and pending.
    pub sent: Vec<Request>,
    /// Fetch error.
    pub error: Option<String>,
    /// uids with an accept / decline / undo in flight.
    pub busy: HashSet<String>,
}

/// One pending group / community invitation from `/api/group/inv-box/list`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupInvite {
    /// Raw numeric group id (without `g_` prefix).
    pub grid: String,
    /// Group / community display name.
    pub name: String,
    /// Group avatar URL.
    pub avatar: String,
    /// Member count.
    pub total_member: usize,
    /// Display name of whoever sent the invite.
    pub inviter_name: String,
    /// Inviter uid.
    pub inviter_id: String,
}

/// "Group & Community Invitations" page state.
#[derive(Clone, Debug, Default)]
pub struct Invites {
    /// Still fetching.
    pub loading: bool,
    /// Pending invitations.
    pub list: Vec<GroupInvite>,
    /// Fetch error.
    pub error: Option<String>,
    /// Group ids with an accept/decline call in flight.
    pub busy: HashSet<String>,
}

/// Parse `/api/group/inv-box/list` JSON into [`GroupInvite`] cards.
#[must_use]
pub fn group_invites_from(value: &Value) -> Vec<GroupInvite> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    root.get("invitations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let info = item.get("groupInfo").unwrap_or(item);
            let grid = s(info, "groupId");
            let grid = if grid.is_empty() {
                s(info, "grid")
            } else {
                grid
            };
            let grid = grid.strip_prefix("g_").unwrap_or(&grid).to_owned();
            if grid.is_empty() {
                return None;
            }
            let name = Some(s(info, "name"))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("Nhóm {grid}"));
            let avatar = s(info, "avt");
            let avatar = if avatar.is_empty() {
                s(info, "avatar")
            } else {
                avatar
            };
            let total_member =
                info.get("totalMember").and_then(Value::as_u64).unwrap_or(0) as usize;
            let inv = item.get("inviterInfo");
            let inviter_id = inv.map(|v| s(v, "userId")).unwrap_or_default();
            let inviter_name = inv
                .map(|v| {
                    Some(s(v, "displayName"))
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| s(v, "zaloName"))
                })
                .unwrap_or_default();
            Some(GroupInvite {
                grid,
                name,
                avatar,
                total_member,
                inviter_name,
                inviter_id,
            })
        })
        .collect()
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned()
}

/// `recommendsv2/list` → received requests.
#[must_use]
pub fn received_from(value: &Value) -> Vec<Request> {
    let root = value.get("data").unwrap_or(value);
    root.get("recommItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let info = item.get("dataInfo")?;
            if info.get("recommType").and_then(Value::as_i64) != Some(2) {
                return None;
            }
            let uid = s(info, "userId");
            if uid.is_empty() {
                return None;
            }
            let name = Some(s(info, "displayName"))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| s(info, "zaloName"));
            Some(Request {
                person: Person {
                    uid,
                    name,
                    avatar: s(info, "avatar"),
                    note: String::new(),
                },
                message: info
                    .get("recommInfo")
                    .map(|r| s(r, "message"))
                    .unwrap_or_default(),
                at_ms: info.get("recommTime").and_then(Value::as_i64).unwrap_or(0),
            })
        })
        .collect()
}

/// `requested/list` → `{uid: {userId, displayName, avatar, fReqInfo}}`.
#[must_use]
pub fn sent_from(value: &Value) -> Vec<Request> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let Some(map) = root.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<Request> = map
        .iter()
        .filter_map(|(key, v)| {
            let info = v.as_object()?;
            let _ = info;
            let uid = Some(s(v, "userId"))
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| key.clone());
            let name = Some(s(v, "displayName"))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| s(v, "zaloName"));
            let req = v.get("fReqInfo");
            Some(Request {
                person: Person {
                    uid,
                    name,
                    avatar: s(v, "avatar"),
                    note: String::new(),
                },
                message: req.map(|r| s(r, "message")).unwrap_or_default(),
                at_ms: req
                    .and_then(|r| r.get("time"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            })
        })
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.at_ms));
    out
}

/// First letter for the A–Z sections (`#` for digits / symbols).
#[must_use]
pub fn section_letter(name: &str) -> String {
    let base = name.chars().next().map(fold_vi).unwrap_or('#');
    if base.is_alphabetic() {
        base.to_uppercase().to_string()
    } else {
        "#".into()
    }
}

/// Strips Vietnamese diacritics (e.g. D-stroke to `D`, accented `A` variants to `A`) for sorting and sections.
fn fold_vi(c: char) -> char {
    let lower = c.to_lowercase().next().unwrap_or(c);
    let base = match lower {
        'à' | 'á' | 'ả' | 'ã' | 'ạ' | 'ă' | 'ằ' | 'ắ' | 'ẳ' | 'ẵ' | 'ặ' | 'â' | 'ầ' | 'ấ' | 'ẩ'
        | 'ẫ' | 'ậ' => 'a',
        'đ' => 'd',
        'è' | 'é' | 'ẻ' | 'ẽ' | 'ẹ' | 'ê' | 'ề' | 'ế' | 'ể' | 'ễ' | 'ệ' => 'e',
        'ì' | 'í' | 'ỉ' | 'ĩ' | 'ị' => 'i',
        'ò' | 'ó' | 'ỏ' | 'õ' | 'ọ' | 'ô' | 'ồ' | 'ố' | 'ổ' | 'ỗ' | 'ộ' | 'ơ' | 'ờ' | 'ớ' | 'ở'
        | 'ỡ' | 'ợ' => 'o',
        'ù' | 'ú' | 'ủ' | 'ũ' | 'ụ' | 'ư' | 'ừ' | 'ứ' | 'ử' | 'ữ' | 'ự' => 'u',
        'ỳ' | 'ý' | 'ỷ' | 'ỹ' | 'ỵ' => 'y',
        other => other,
    };
    if c.is_uppercase() {
        base.to_ascii_uppercase()
    } else {
        base
    }
}

/// Lowercase, accent-free key for accent-insensitive search matching.
#[must_use]
pub fn fold_key(text: &str) -> String {
    text.chars()
        .map(|c| fold_vi(c).to_ascii_lowercase())
        .collect()
}

/// `dd/mm` from unix ms (request cards).
fn short_date(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let (_, m, d) = crate::data::vn_ymd(ms);
    format!("{d:02}/{m:02}")
}

impl ShellView {
    /// Friends from `get_friends` (fallback: 1-1 threads), groups from the
    /// group profiles (fallback: group threads). `(uid, name, members)`.
    pub(crate) fn contact_rows(&self, groups: bool) -> Vec<(String, String, u32)> {
        let mut rows: Vec<(String, String, u32)> = self
            .profiles
            .map
            .iter()
            .filter(|(k, p)| {
                if groups {
                    k.starts_with("g_")
                } else {
                    p.friend && !k.starts_with("g_")
                }
            })
            .filter(|(_, p)| !p.name.is_empty())
            .map(|(k, p)| (k.clone(), p.name.clone(), p.members))
            .collect();
        if rows.is_empty() {
            let src = if groups { &self.groups } else { &self.friends };
            rows = src
                .iter()
                .map(|r| (r.uid.clone(), r.name.clone(), 0))
                .collect();
        }
        let q = fold_key(self.contacts_q.trim());
        if !q.is_empty() {
            rows.retain(|(_, name, _)| fold_key(name).contains(&q));
        }
        if let Some(lid) = self.contacts_label {
            rows.retain(|(uid, _, _)| self.thread_label(uid) == Some(lid));
        }
        if groups && !self.contacts_by_name {
            // "Activity (newest → oldest)": order of the conversation list.
            let pos = |uid: &str| {
                self.convs
                    .iter()
                    .position(|c| c.thread_id == uid)
                    .unwrap_or(usize::MAX)
            };
            rows.sort_by_key(|(uid, _, _)| pos(uid));
        } else {
            rows.sort_by_key(|(_, name, _)| fold_key(name));
            if self.contacts_desc {
                rows.reverse();
            }
        }
        rows
    }

    /// "Friends (n)" / "Groups & Communities (n)" card.
    pub(crate) fn contacts_list(&self, groups: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
        let rows = self.contact_rows(groups);
        let heading = if groups {
            format!("Nhóm và cộng đồng ({})", rows.len())
        } else {
            format!("Bạn bè ({})", rows.len())
        };
        let sort_label = if groups {
            if self.contacts_by_name {
                "Tên (A-Z)"
            } else {
                "Hoạt động (mới → cũ)"
            }
        } else if self.contacts_desc {
            "Tên (Z-A)"
        } else {
            "Tên (A-Z)"
        };
        let mut items: Vec<gpui::AnyElement> = Vec::new();
        let mut letter = String::new();
        let lettered = !groups || self.contacts_by_name;
        for (uid, name, members) in rows {
            if lettered {
                let l = section_letter(&name);
                if l != letter {
                    letter = l.clone();
                    items.push(
                        div()
                            .flex_none()
                            .px(px(20.))
                            .pt(px(14.))
                            .pb(px(6.))
                            .text_size(px(15.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child(l)
                            .into_any_element(),
                    );
                }
            }
            let person = Person {
                uid: uid.clone(),
                name: name.clone(),
                avatar: String::new(),
                note: String::new(),
            };
            let (open, open_name, menu_name, menu_uid, more_name, more_uid) = (
                uid.clone(),
                name.clone(),
                name.clone(),
                uid.clone(),
                name.clone(),
                uid.clone(),
            );
            let row_key = crate::data::hash_id(&uid);
            let row_group = gpui::SharedString::from(format!("contact-row-{row_key}"));
            items.push(
                div()
                    .id(("contact-row", row_key))
                    .group(row_group)
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .h(px(64.))
                    .px(px(20.))
                    .cursor_pointer()
                    .hover(|st| st.bg(rgb(hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_contact(&open, &open_name, groups, window, cx);
                        cx.notify();
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, window, cx| {
                            this.open_menu(
                                crate::context_menu::MenuKind::Contact {
                                    name: menu_name.clone(),
                                    is_group: groups,
                                    uid: menu_uid.clone(),
                                },
                                e.position,
                                window,
                            );
                            cx.notify();
                        }),
                    )
                    .child({
                        let show = !groups && self.uid_online(&uid);
                        div()
                            .relative()
                            .flex_none()
                            .size(px(48.))
                            .child(self.dialog_avatar(&person, 48.))
                            .when(show, |el| {
                                el.child(
                                    div()
                                        .absolute()
                                        .right_0()
                                        .bottom_0()
                                        .size(px(12.))
                                        .rounded(px(6.))
                                        .bg(rgb(0x2fcc70))
                                        .border_2()
                                        .border_color(rgb(0xffffff)),
                                )
                            })
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .text_color(rgb(ink))
                                    .truncate()
                                    .child(name),
                            )
                            .when(groups && members > 0, |el| {
                                el.child(
                                    div()
                                        .text_size(px(13.))
                                        .text_color(rgb(muted))
                                        .child(crate::i18n::members(members)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id(("contact-more", row_key))
                            .flex_none()
                            .size(px(28.))
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(rgb(muted))
                            .hover(move |st| st.bg(rgb(field)).text_color(rgb(ink)))
                            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_menu(
                                    crate::context_menu::MenuKind::Contact {
                                        name: more_name.clone(),
                                        is_group: groups,
                                        uid: more_uid.clone(),
                                    },
                                    e.position(),
                                    window,
                                );
                                cx.notify();
                            }))
                            .child(fonts::icon_el('\u{EC66}', "⋯", 16.)),
                    )
                    .into_any_element(),
            );
        }
        let empty = items.is_empty();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .px(px(16.))
            .pt(px(22.))
            .child(
                div()
                    .flex_none()
                    .text_size(px(15.5))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child(heading),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .mt(px(16.))
                    .mb(px(16.))
                    .rounded(px(8.))
                    .bg(rgb(paper))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .p(px(16.))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.))
                                    .flex_1()
                                    .h(px(32.))
                                    .px(px(10.))
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(crate::color::border_subtle())
                                    .text_color(rgb(muted))
                                    .child(fonts::icon_el('\u{ED15}', "⌕", 14.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(14.))
                                            .text_color(rgb(ink))
                                            .child(self.contacts_query.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .id("contacts-sort")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.))
                                    .h(px(32.))
                                    .px(px(12.))
                                    .rounded(px(4.))
                                    .bg(rgb(field))
                                    .hover(|st| st.bg(rgb(hover)))
                                    .cursor_pointer()
                                    .text_size(px(14.))
                                    .text_color(rgb(ink))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        if groups {
                                            this.contacts_by_name = !this.contacts_by_name;
                                        } else {
                                            this.contacts_desc = !this.contacts_desc;
                                        }
                                        cx.notify();
                                    }))
                                    .child(fonts::icon_el('\u{EBA0}', "⇅", 16.))
                                    .child(sort_label)
                                    .child(
                                        div()
                                            .text_color(rgb(muted))
                                            .child(fonts::icon_el(
                                                fonts::ic::SOLID_DOWN,
                                                "▾",
                                                12.,
                                            )),
                                    ),
                            )
                            .child({
                                let active_label = self.contacts_label.and_then(|lid| {
                                    self.labels.iter().find(|l| l.id == lid)
                                });
                                let filter_text = active_label
                                    .map(|l| l.text.clone())
                                    .unwrap_or_else(|| "Tất cả".to_owned());
                                let filter_dot = active_label
                                    .and_then(|l| crate::color::parse_hex(&l.color));
                                div()
                                    .id("contacts-filter")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.))
                                    .h(px(32.))
                                    .px(px(12.))
                                    .rounded(px(4.))
                                    .bg(rgb(field))
                                    .hover(|st| st.bg(rgb(hover)))
                                    .cursor_pointer()
                                    .text_size(px(14.))
                                    .text_color(rgb(ink))
                                    .on_click(cx.listener(|this, e: &ClickEvent, window, cx| {
                                        this.open_menu(
                                            crate::context_menu::MenuKind::ContactFilter,
                                            e.position(),
                                            window,
                                        );
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .text_color(rgb(muted))
                                            .child(fonts::icon_el('\u{EB31}', "⏷", 15.)),
                                    )
                                    .when_some(filter_dot, |el, dot| {
                                        el.child(
                                            div()
                                                .size(px(8.))
                                                .rounded(px(4.))
                                                .bg(rgb(dot)),
                                        )
                                    })
                                    .child(filter_text)
                                    .child(
                                        div()
                                            .text_color(rgb(muted))
                                            .child(fonts::icon_el(
                                                fonts::ic::SOLID_DOWN,
                                                "▾",
                                                12.,
                                            )),
                                    )
                            }),
                    )
                    .child(
                        div()
                            .id("contact-people")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h(px(0.))
                            .pb(px(8.))
                            .overflow_y_scroll()
                            .when(empty, |el| {
                                el.child(
                                    div()
                                        .p(px(24.))
                                        .text_size(px(14.))
                                        .text_color(rgb(muted))
                                        .child(
                                            if self.contacts_q.trim().is_empty()
                                                && self.contacts_label.is_none()
                                            {
                                                "Chưa có dữ liệu"
                                            } else {
                                                "Không tìm thấy kết quả"
                                            },
                                        ),
                                )
                            })
                            .children(items),
                    ),
            )
    }

    /// Open the conversation of a contact row (friend uid / `g_` grid), creating a live row if needed.
    fn open_contact(
        &mut self,
        uid: &str,
        name: &str,
        is_group: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chat_or_note(
            uid,
            name,
            is_group,
            "Chưa có hội thoại với liên hệ này trên máy",
            window,
            cx,
        );
    }

    /// Fetch received + sent requests (Contacts › Friend Requests).
    pub(crate) fn load_requests(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            self.requests = Some(Requests {
                error: Some("Chưa đăng nhập".into()),
                ..Requests::default()
            });
            return;
        };
        if self.requests.as_ref().is_some_and(|r| r.loading) {
            return;
        }
        self.requests = Some(Requests {
            loading: true,
            ..Requests::default()
        });
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let received = api
                            .get_recommended_friends()
                            .await
                            .map(|v| received_from(&v))
                            .map_err(|e| e.to_string())?;
                        // `requested/list` errors when nothing is pending.
                        let sent = api
                            .get_sent_requests()
                            .await
                            .map(|v| sent_from(&v))
                            .unwrap_or_default();
                        Ok((received, sent))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let mut st = Requests::default();
                match got {
                    Ok((received, sent)) => {
                        for r in received.iter().chain(sent.iter()) {
                            view.profiles
                                .map
                                .entry(r.person.uid.clone())
                                .or_insert_with(|| crate::profiles::Profile {
                                    name: r.person.name.clone(),
                                    avatar: r.person.avatar.clone(),
                                    ..crate::profiles::Profile::default()
                                });
                        }
                        view.avatar_all = true;
                        view.avatars_queued = true;
                        st.received = received;
                        st.sent = sent;
                    }
                    Err(e) => st.error = Some(e),
                }
                view.requests = Some(st);
                cx.notify();
            });
        })
        .detach();
    }

    fn answer(&mut self, uid: String, action: zalo_client::FriendAnswer, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        if let Some(r) = self.requests.as_mut() {
            if !r.busy.insert(uid.clone()) {
                return;
            }
        }
        let key = uid.clone();
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.answer_friend_request(&key, action)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(r) = view.requests.as_mut() {
                    r.busy.remove(&uid);
                    match &got {
                        Ok(_) => {
                            r.received.retain(|x| x.person.uid != uid);
                            r.sent.retain(|x| x.person.uid != uid);
                        }
                        Err(e) => r.error = Some(e.clone()),
                    }
                }
                view.set_note(match (&got, action) {
                    (Ok(_), zalo_client::FriendAnswer::Accept) => "Đã đồng ý kết bạn".to_owned(),
                    (Ok(_), zalo_client::FriendAnswer::Reject) => "Đã từ chối lời mời".to_owned(),
                    (Ok(_), zalo_client::FriendAnswer::Undo) => "Đã thu hồi lời mời".to_owned(),
                    (Err(e), _) => format!("Không thực hiện được: {e}"),
                });
                if matches!((&got, action), (Ok(_), zalo_client::FriendAnswer::Accept)) {
                    view.profiles_refresh = true;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Friend Requests": received cards with Decline / Accept, sent cards
    /// with "Revoke request".
    pub(crate) fn requests_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let st = self.requests.clone().unwrap_or_default();
        let card =
            |key: String, r: &Request, received: bool, busy: bool, cx: &mut Context<Self>| {
                let (a, b) = (r.person.uid.clone(), r.person.uid.clone());
                let btn = |id: String, label: &'static str, primary: bool| {
                    div()
                        .id(gpui::SharedString::from(id))
                        .flex_1()
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .text_size(px(14.))
                        .font_family(FAMILY_SEMIBOLD)
                        .cursor_pointer()
                        .when(primary, |el| {
                            el.bg(rgb(token_rgb("--B10", "#e5efff")))
                                .text_color(rgb(blue))
                        })
                        .when(!primary, |el| {
                            el.bg(rgb(token_rgb(
                                "--button-secondary-neutral-normal",
                                "#e5e7eb",
                            )))
                            .text_color(rgb(ink))
                        })
                        .when(busy, |el| el.opacity(0.5))
                        .child(label)
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .w(px(300.))
                    .p(px(16.))
                    .rounded(px(8.))
                    .bg(rgb(paper))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .child(self.dialog_avatar(&r.person, 48.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w(px(0.))
                                    .child(
                                        div()
                                            .text_size(px(15.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(ink))
                                            .truncate()
                                            .child(r.person.name.clone()),
                                    )
                                    .child(div().text_size(px(13.)).text_color(rgb(muted)).child(
                                        if received {
                                            format!("{} · Muốn kết bạn", short_date(r.at_ms))
                                        } else {
                                            format!("{} · Đã gửi lời mời", short_date(r.at_ms))
                                        },
                                    )),
                            ),
                    )
                    .when(!r.message.is_empty(), |el| {
                        el.child(
                            div()
                                .p(px(10.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(crate::color::border_subtle())
                                .text_size(px(14.))
                                .text_color(rgb(ink))
                                .child(crate::media::wrap_bubble_text(&r.message, 250.)),
                        )
                    })
                    .child(if received {
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .child(btn(format!("{key}-no"), "Từ chối", false).on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.answer(a.clone(), zalo_client::FriendAnswer::Reject, cx);
                                    cx.notify();
                                }),
                            ))
                            .child(
                                btn(format!("{key}-yes"), "Đồng ý", true).on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.answer(
                                            b.clone(),
                                            zalo_client::FriendAnswer::Accept,
                                            cx,
                                        );
                                        cx.notify();
                                    },
                                )),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .child(
                                btn(format!("{key}-undo"), "Thu hồi lời mời", false).on_click(
                                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.answer(a.clone(), zalo_client::FriendAnswer::Undo, cx);
                                        cx.notify();
                                    }),
                                ),
                            )
                            .into_any_element()
                    })
            };
        let section = |title: String| {
            div()
                .flex_none()
                .pt(px(22.))
                .pb(px(12.))
                .text_size(px(15.5))
                .font_family(FAMILY_SEMIBOLD)
                .text_color(rgb(ink))
                .child(title)
        };
        let grid = |cards: Vec<gpui::AnyElement>| {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(16.))
                .children(cards)
        };
        let received: Vec<gpui::AnyElement> = st
            .received
            .iter()
            .enumerate()
            .map(|(i, r)| {
                card(
                    format!("req-in-{i}"),
                    r,
                    true,
                    st.busy.contains(&r.person.uid),
                    cx,
                )
                .into_any_element()
            })
            .collect();
        let sent: Vec<gpui::AnyElement> = st
            .sent
            .iter()
            .enumerate()
            .map(|(i, r)| {
                card(
                    format!("req-out-{i}"),
                    r,
                    false,
                    st.busy.contains(&r.person.uid),
                    cx,
                )
                .into_any_element()
            })
            .collect();
        let empty_note =
            |text: &'static str| div().text_size(px(14.)).text_color(rgb(muted)).child(text);
        div()
            .id("requests-page")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .px(px(16.))
            .pb(px(16.))
            .overflow_y_scroll()
            .when(st.loading, |el| {
                el.child(
                    div()
                        .pt(px(22.))
                        .text_size(px(14.))
                        .text_color(rgb(muted))
                        .child("Đang tải…"),
                )
            })
            .when_some(st.error.clone(), |el, e| {
                el.child(
                    div()
                        .pt(px(22.))
                        .text_size(px(14.))
                        .text_color(rgb(muted))
                        .child(e),
                )
            })
            .child(section(format!("Lời mời đã nhận ({})", st.received.len())))
            .child(if received.is_empty() {
                empty_note("Bạn không có lời mời nào").into_any_element()
            } else {
                grid(received).into_any_element()
            })
            .child(section(format!("Lời mời đã gửi ({})", st.sent.len())))
            .child(if sent.is_empty() {
                empty_note("Chưa gửi lời mời nào").into_any_element()
            } else {
                grid(sent).into_any_element()
            })
    }

    /// Fetch `/api/group/inv-box/list` for "Group & Community Invitations".
    pub(crate) fn load_invites(&mut self, cx: &mut Context<Self>) {
        if self.invites.as_ref().is_some_and(|st| st.loading) {
            return;
        }
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            self.invites = Some(Invites {
                error: Some("Đăng nhập để xem lời mời vào nhóm".into()),
                ..Invites::default()
            });
            return;
        };
        let mut st = self.invites.take().unwrap_or_default();
        st.loading = true;
        st.error = None;
        self.invites = Some(st);
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let v = api
                            .get_group_invites(0, 30)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(group_invites_from(&v))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let mut st = Invites::default();
                match got {
                    Ok(list) => {
                        for inv in &list {
                            let tid = format!("g_{}", inv.grid);
                            view.profiles.map.entry(tid).or_insert_with(|| {
                                crate::profiles::Profile {
                                    name: inv.name.clone(),
                                    avatar: inv.avatar.clone(),
                                    members: inv.total_member as u32,
                                    ..crate::profiles::Profile::default()
                                }
                            });
                        }
                        view.avatar_all = true;
                        view.avatars_queued = true;
                        st.list = list;
                    }
                    Err(e) => st.error = Some(e),
                }
                view.invites = Some(st);
                cx.notify();
            });
        })
        .detach();
    }

    fn answer_invite(&mut self, grid: String, name: String, join: bool, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        if let Some(st) = self.invites.as_mut() {
            if !st.busy.insert(grid.clone()) {
                return;
            }
        }
        let key = grid.clone();
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        if join {
                            api.accept_group_invite(&key)
                                .await
                                .map_err(|e| e.to_string())
                        } else {
                            let payload = [serde_json::json!({ "grid": key })];
                            api.decline_group_invites(&payload, false)
                                .await
                                .map_err(|e| e.to_string())
                        }
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(st) = view.invites.as_mut() {
                    st.busy.remove(&grid);
                    if got.is_ok() {
                        st.list.retain(|item| item.grid != grid);
                    } else if let Err(e) = &got {
                        st.error = Some(e.clone());
                    }
                }
                match got {
                    Ok(_) if join => {
                        view.profiles_refresh = true;
                        view.set_note(format!("Đã tham gia nhóm «{name}»"));
                    }
                    Ok(_) => view.set_note(format!("Đã từ chối lời mời «{name}»")),
                    Err(e) => view.set_note(format!("Không xử lý được lời mời: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Right-hand pane for "Group & Community Invitations".
    pub(crate) fn invites_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let card_bg = token_rgb("--layer-background", "#ffffff");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let blue = token_rgb("--B55", "#0068ff");
        let st = self.invites.as_ref();
        let list = st.map(|s| s.list.as_slice()).unwrap_or(&[]);
        let loading = st.is_some_and(|s| s.loading);
        let err = st.and_then(|s| s.error.clone());
        let busy = st.map(|s| &s.busy);
        div()
            .id("contacts-invites")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .p(px(16.))
            .gap(px(12.))
            .overflow_y_scroll()
            .when(loading, |el| {
                el.child(
                    div()
                        .text_size(px(14.))
                        .text_color(rgb(muted))
                        .child("Đang tải lời mời vào nhóm…"),
                )
            })
            .when_some(err, |el, e| {
                el.child(div().text_size(px(13.5)).text_color(rgb(muted)).child(e))
            })
            .child(
                div()
                    .text_size(px(14.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child(format!("Lời mời vào nhóm và cộng đồng ({})", list.len())),
            )
            .when(list.is_empty() && !loading, |el| {
                el.child(
                    div()
                        .py(px(36.))
                        .text_size(px(14.))
                        .text_color(rgb(muted))
                        .child("Bạn không có lời mời vào nhóm hoặc cộng đồng nào."),
                )
            })
            .children(list.iter().enumerate().map(|(i, inv)| {
                let grid = inv.grid.clone();
                let name_join = inv.name.clone();
                let name_dec = inv.name.clone();
                let grid_dec = inv.grid.clone();
                let is_busy = busy.is_some_and(|b| b.contains(&inv.grid));
                let subtitle = if inv.inviter_name.is_empty() {
                    format!("{} thành viên", inv.total_member)
                } else {
                    format!(
                        "{} thành viên · Mời bởi {}",
                        inv.total_member, inv.inviter_name
                    )
                };
                div()
                    .id(("inv-card", i as u64))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .p(px(14.))
                    .rounded(px(8.))
                    .bg(rgb(card_bg))
                    .border_1()
                    .border_color(crate::color::border_subtle())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .min_w(px(0.))
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(ink))
                                    .truncate()
                                    .child(inv.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(muted))
                                    .truncate()
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id(("inv-decline", i as u64))
                                    .px(px(14.))
                                    .h(px(34.))
                                    .rounded(px(4.))
                                    .bg(rgb(subtle))
                                    .text_color(rgb(ink))
                                    .text_size(px(13.5))
                                    .flex()
                                    .items_center()
                                    .when(!is_busy, |el| el.cursor_pointer())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !is_busy {
                                            this.answer_invite(
                                                grid_dec.clone(),
                                                name_dec.clone(),
                                                false,
                                                cx,
                                            );
                                        }
                                    }))
                                    .child("Từ chối"),
                            )
                            .child(
                                div()
                                    .id(("inv-join", i as u64))
                                    .px(px(14.))
                                    .h(px(34.))
                                    .rounded(px(4.))
                                    .bg(rgb(blue))
                                    .text_color(rgb(0xff_ff_ff))
                                    .text_size(px(13.5))
                                    .flex()
                                    .items_center()
                                    .when(!is_busy, |el| el.cursor_pointer())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !is_busy {
                                            this.answer_invite(
                                                grid.clone(),
                                                name_join.clone(),
                                                true,
                                                cx,
                                            );
                                        }
                                    }))
                                    .child(if is_busy {
                                        "Đang xử lý…"
                                    } else {
                                        "Tham gia"
                                    }),
                            ),
                    )
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_sections() {
        let rec = serde_json::json!({"data": {"recommItems": [
            {"dataInfo": {"userId": "1", "displayName": "An", "recommType": 2, "recommTime": 5, "recommInfo": {"message": "Chào"}}},
            {"dataInfo": {"userId": "2", "zaloName": "Bình", "recommType": 1}}
        ]}});
        let r = received_from(&rec);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].person.name.as_str(), r[0].message.as_str()),
            ("An", "Chào")
        );
        let sent = serde_json::json!({"9": {"userId": "9", "displayName": "Chi", "fReqInfo": {"message": "Hi", "time": 7}}});
        let s = sent_from(&sent);
        assert_eq!((s[0].person.uid.as_str(), s[0].at_ms), ("9", 7));
        assert_eq!(section_letter("Đức"), "D");
        assert_eq!(section_letter("Ánh"), "A");
        assert_eq!(section_letter("9x"), "#");
        assert_eq!(fold_key("Nguyễn"), "nguyen");
    }

    #[test]
    fn parses_group_invites() {
        let raw = serde_json::json!({
            "data": {
                "invitations": [{
                    "groupInfo": {
                        "groupId": "8899",
                        "name": "Nhóm Rust",
                        "avt": "https://a/g.jpg",
                        "totalMember": 15
                    },
                    "inviterInfo": {
                        "userId": "42",
                        "displayName": "Minh"
                    }
                }]
            }
        });
        let list = group_invites_from(&raw);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].grid, "8899");
        assert_eq!(list[0].name, "Nhóm Rust");
        assert_eq!(list[0].total_member, 15);
        assert_eq!(list[0].inviter_name, "Minh");
    }
}
