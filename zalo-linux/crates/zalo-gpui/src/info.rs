//! "Group Info" / "Conversation Info" column, after the live Zalo PC
//! panel (CDP 11/11/2026): 344 wide, centered title, avatar + name, four
//! round actions, then "Group Members", "Group Board", "Photos/Videos",
//! "Files", "Links", "Security Settings" (hide conversation, report),
//! "Delete Chat History" / "Leave Group". A pending-join row appears
//! when `/api/group/pending-mems/list` returns people. Sections are
//! separated by 8 px `--layer-background-subtle`.

use gpui::{
    div, img, prelude::*, px, rgb, ClickEvent, Context, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};

use crate::color::token_rgb;
use crate::data::ChatConv;
use crate::fonts::{self, ic, FAMILY_SEMIBOLD};
use crate::media::MsgKind;
use crate::shell::ShellView;
use crate::window_frame::CsdCorners;

/// Right-hand info column, same width as the conversation list inner.
pub const INFO_PANEL_W: f32 = 344.0;
/// Electron child-chat window: `innerWidth > 824` is `COMPACT_LEVEL.NORMAL`.
pub const INFO_DOCK_MIN_SOLO: f32 = 824.0;
/// Electron `MIN_WIDTH_CHAT_AREA` so the docked column still leaves a chat.
const INFO_CHAT_MIN: f32 = 420.0;

/// True when the 344 px column fits beside the chat instead of covering it.
pub fn info_docks(width: f32, solo: bool) -> bool {
    let need = if solo {
        INFO_DOCK_MIN_SOLO
    } else {
        zalo_design::MAIN_TAB_W + zalo_design::CONV_LIST_INNER_W + INFO_CHAT_MIN + INFO_PANEL_W
    };
    width > need
}

mod g {
    pub const MUTE: char = '\u{EC74}';
    pub const UNMUTE: char = '\u{EC76}';
    pub const PIN: char = '\u{ECCB}';
    pub const UNPIN: char = '\u{EDE1}';
    pub const ADD: char = '\u{EB45}';
    pub const SETTING: char = '\u{ED22}';
    pub const EDIT: char = '\u{EBC3}';
    pub const MEMBERS: char = '\u{EB44}';
    pub const CLOCK: char = '\u{EA9C}';
    pub const NOTE: char = '\u{EDAA}';
    pub const CHEVRON: char = '\u{EA8B}';
    pub const FILE: char = '\u{EB1B}';
    pub const LINK: char = '\u{EC43}';
    pub const HIDE: char = '\u{EB11}';
    pub const TRASH: char = '\u{EDCF}';
    pub const LEAVE: char = '\u{EBC4}';
    pub const WARN: char = '\u{EE04}';
}

/// "Members" sub-page of the info column (live `getgi/v2`).
#[derive(Clone, Debug, Default)]
pub struct MembersView {
    /// Conversation row id.
    pub conv: u64,
    /// Still fetching.
    pub loading: bool,
    /// `(uid, name)` in server order.
    pub list: Vec<(String, String)>,
    /// `creatorId` — "Group Owner".
    pub creator: String,
    /// `adminIds` — "Group Admin".
    pub admins: Vec<String>,
    /// Members currently being updated/removed via group admin APIs.
    pub kicking: std::collections::HashSet<String>,
    /// Second-click confirmation target for transferring group ownership.
    pub owner_armed: Option<String>,
    /// Fetch error.
    pub error: Option<String>,
}

/// Role mutation requested from the "Members" panel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupRoleAction {
    PromoteAdmin,
    DemoteAdmin,
    TransferOwner,
}

/// Returns true when `mine` (creator only) can promote/demote or transfer ownership to `target_uid`.
#[must_use]
pub(crate) fn can_manage_group_roles(
    mine: &std::collections::HashSet<String>,
    creator: &str,
    target_uid: &str,
) -> bool {
    !target_uid.is_empty()
        && !creator.is_empty()
        && mine.contains(creator)
        && !mine.contains(target_uid)
        && target_uid != creator
}

/// Returns true when `mine` has permission to kick `target_uid` from a group.
#[must_use]
pub(crate) fn can_kick_member(
    mine: &std::collections::HashSet<String>,
    creator: &str,
    admins: &[String],
    target_uid: &str,
) -> bool {
    let target = target_uid.trim();
    if target.is_empty() || mine.contains(target) || target == creator {
        return false;
    }
    let is_creator = !creator.is_empty() && mine.contains(creator);
    let is_admin = admins.iter().any(|id| mine.contains(id));
    is_creator || (is_admin && !admins.iter().any(|id| id == target))
}

/// Pending-member list is owner/admin only. An empty creator means the role
/// is not known yet, so the caller must not hit the pending API.
#[must_use]
pub(crate) fn may_list_group_pending(
    mine: &std::collections::HashSet<String>,
    creator: &str,
    admins: &[String],
) -> bool {
    !creator.is_empty() && crate::account::group_role_label(mine, creator, admins) != "Thành viên"
}

/// Creator / admins / members from a decrypted `getgi/v2` reply.
pub(crate) fn members_from(
    value: &serde_json::Value,
) -> (Vec<(String, String, String)>, String, Vec<String>) {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let info = root
        .get("gridInfoMap")
        .and_then(serde_json::Value::as_object)
        .and_then(|m| m.values().next())
        .unwrap_or(root);
    let list = zalo_client::GroupMember::list_from(value)
        .into_iter()
        .map(|m| (m.uid, m.name, m.avatar.unwrap_or_default()))
        .collect();
    let creator = info
        .get("creatorId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    let admins = info
        .get("adminIds")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    (list, creator, admins)
}

#[derive(Clone, Copy)]
enum InfoAct {
    Members,
    Rename,
    Mute,
    Pin,
    AddMember,
    CreateGroup,
    Manage,
    Pending,
    Hide,
    Clear,
    Leave,
    LeaveSilent,
    Report,
    Alias,
    Board(crate::board::BoardTab),
}

/// Electron-style permission switch. The parent row owns the click.
pub(crate) fn switch_pill(on: bool) -> gpui::AnyElement {
    let off_bg = if crate::color::is_dark() {
        0x4b_53_5e
    } else {
        0xc5_ca_d3
    };
    div()
        .flex_none()
        .w(px(36.))
        .h(px(20.))
        .px(px(2.))
        .rounded(px(10.))
        .flex()
        .items_center()
        .bg(rgb(if on {
            token_rgb("--B55", "#0068ff")
        } else {
            off_bg
        }))
        .when(on, |el| el.justify_end())
        .when(!on, |el| el.justify_start())
        .child(div().size(px(16.)).rounded(px(8.)).bg(rgb(0xffffff)))
        .into_any_element()
}

/// Small checkbox for "Leave group silently".
pub(crate) fn check_box(on: bool) -> gpui::AnyElement {
    div()
        .flex_none()
        .size(px(16.))
        .rounded(px(3.))
        .border_1()
        .border_color(crate::color::border_subtle())
        .bg(rgb(if on {
            token_rgb("--B55", "#0068ff")
        } else {
            token_rgb("--layer-background", "#ffffff")
        }))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .text_color(rgb(0xffffff))
        .child(if on { "✓" } else { "" })
        .into_any_element()
}

impl ShellView {
    /// `#conversation-info`. `overlay` paints it over the chat (narrow window).
    pub(crate) fn info_panel(
        &self,
        conv: Option<&ChatConv>,
        corners: CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let Some(conv) = conv.cloned() else {
            return div().into_any_element();
        };
        if let Some(view) = self.members_view.as_ref().filter(|m| m.conv == conv.id) {
            return self
                .members_panel(&conv, view, corners, cx)
                .into_any_element();
        }
        if let Some(crate::dialogs::Dialog::Board(b)) = self.dialog.as_ref() {
            return self.board_modal(b, corners, cx).into_any_element();
        }
        if let Some(crate::dialogs::Dialog::GroupManage(g)) = self.dialog.as_ref() {
            return self.group_manage_el(g, corners, cx).into_any_element();
        }
        let is_group = conv.is_group;
        let id = conv.id;
        let members = self
            .members_known(&conv.thread_id)
            .map(|n| n as usize)
            .unwrap_or_else(|| self.members_of(&conv).len().max(1));
        let msgs = self.messages_of(id);
        let photos: Vec<_> = msgs
            .iter()
            .rev()
            .filter(|m| {
                matches!(
                    m.kind,
                    MsgKind::Photo | MsgKind::Doodle | MsgKind::Gif | MsgKind::Video
                )
            })
            .take(8)
            .cloned()
            .collect();
        let photo_cell = ((INFO_PANEL_W - 32.0 - 4.0 * 3.0) / 4.0).floor();
        let pending_n = self.group_pending_n.get(&id).copied().unwrap_or(0);
        let e2ee_line = if is_group {
            if self
                .e2ee_groups
                .as_ref()
                .is_some_and(|ids| zalo_client::features::group_marked_e2ee(ids, &conv.thread_id))
            {
                Some(
                    "Máy chủ đánh dấu nhóm này là mã hóa đầu cuối. Máy này chưa mã hóa nội dung tin gửi đi.",
                )
            } else if self.e2ee_failed {
                Some("Chưa đọc được danh sách nhóm mã hóa đầu cuối.")
            } else {
                None
            }
        } else {
            None
        };
        let member_preview: Vec<String> = if is_group {
            self.profiles
                .map
                .get(&conv.thread_id)
                .map(|p| {
                    p.member_ids
                        .iter()
                        .filter(|uid| !uid.is_empty())
                        .take(6)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let files: Vec<_> = msgs
            .iter()
            .rev()
            .filter(|m| m.kind == MsgKind::File)
            .take(3)
            .cloned()
            .collect();
        let links: Vec<_> = msgs
            .iter()
            .rev()
            .filter_map(|m| crate::media::first_link(&m.text).map(|u| (u, m.time.clone())))
            .take(3)
            .collect();
        let cloud_photos = self.cloud_photos(&conv.thread_id);
        let cloud_files = self.cloud_files(&conv.thread_id);
        let cloud_links = self.cloud_links(&conv.thread_id);
        let use_cloud_photos = !cloud_photos.is_empty();
        let use_cloud_files = !cloud_files.is_empty();
        let use_cloud_links = !cloud_links.is_empty();
        let store_thread = conv.thread_id.clone();
        let muted_on = self.muted.contains(&id);
        let pinned_on = self.pinned.contains(&id);
        let action = |this: &Self,
                      key: &'static str,
                      code: char,
                      label: &'static str,
                      act: InfoAct,
                      cx: &mut Context<Self>| {
            let _ = this;
            div()
                .id(key)
                .flex()
                .flex_col()
                .items_center()
                .w(px(80.))
                .gap(px(6.))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.info_action(id, act, cx);
                    cx.notify();
                }))
                .child({
                    let circle_bg = if crate::color::is_dark() {
                        0x2f_35_3d
                    } else {
                        token_rgb("--layer-background-subtle", "#ebecf0")
                    };
                    div()
                        .size(px(32.))
                        .rounded(px(16.))
                        .bg(rgb(circle_bg))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(fonts::icon_el(code, "•", 20.))
                })
                // Two fixed lines like Zalo PC ("Mute / notifications"): GPUI's own
                // wrapping breaks Vietnamese words apart.
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .children(
                            crate::i18n::t(label)
                                .split('\n')
                                .map(|part| div().whitespace_nowrap().child(part.to_owned()))
                                .collect::<Vec<_>>(),
                        ),
                )
        };
        let section = |title: &'static str| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .flex_none()
                .px(px(16.))
                .h(px(48.))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_family(FAMILY_SEMIBOLD)
                        .child(crate::i18n::t(title.trim())),
                )
                .child(fonts::icon_el(g::CHEVRON, "▾", 16.))
        };
        let line = |key: String,
                    code: char,
                    text: String,
                    act: Option<InfoAct>,
                    danger: bool,
                    cx: &mut Context<Self>| {
            let color = if danger {
                token_rgb("--R60", "#c31818")
            } else {
                ink
            };
            div()
                .id(gpui::SharedString::from(key))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.))
                .flex_none()
                .px(px(16.))
                .h(px(48.))
                .text_size(px(15.))
                .text_color(rgb(color))
                .when_some(act, |el, act| {
                    el.cursor_pointer()
                        .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.info_action(id, act, cx);
                            cx.notify();
                        }))
                })
                .child(fonts::icon_el(code, "•", 20.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .child(crate::i18n::t(&text)),
                )
        };
        let gap = || div().h(px(8.)).flex_none().bg(rgb(subtle));
        let cloud =
            crate::shell::is_my_documents(&conv.name, &conv.thread_id, self.send2me_id.as_deref());
        let avatar = match self.avatar_of(&conv.thread_id) {
            Some(i) => div()
                .size(px(56.))
                .rounded(px(28.))
                .overflow_hidden()
                .border_1()
                .border_color(crate::color::border_subtle())
                .bg(rgb(crate::color::token_rgb(
                    "--layer-background",
                    "#ffffff",
                )))
                .child(
                    img(i)
                        .size_full()
                        .rounded(px(28.))
                        .object_fit(gpui::ObjectFit::Cover),
                )
                .into_any_element(),
            None if cloud => div()
                .size(px(56.))
                .rounded(px(28.))
                .bg(rgb(token_rgb("--B55", "#0068ff")))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0xffffff))
                .child(fonts::icon_el(ic::DOCS_ON, "☁", 38.))
                .into_any_element(),
            None => div()
                .size(px(56.))
                .rounded(px(28.))
                .bg(rgb(token_rgb("--B55", "#0068ff")))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0xffffff))
                .text_size(px(22.))
                .font_family(FAMILY_SEMIBOLD)
                .child(conv.name.chars().next().unwrap_or('?').to_string())
                .into_any_element(),
        };
        let leave_armed = self.leave_armed == Some(id);
        corners
            .apply(
                div()
                    .id("conversation-info")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w(px(INFO_PANEL_W))
                    .h_full()
                    .bg(rgb(paper))
                    .border_l_1()
                    .border_color(crate::color::border_subtle())
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
                    .items_center()
                    .justify_center()
                    .h(px(zalo_design::CHAT_HEADER_H))
                    .border_b_1()
                    .border_color(crate::color::border_subtle())
                    .text_size(px(18.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child(crate::i18n::t(if is_group {
                        crate::color::str_vi("STR_PROFILE_GROUP", "Thông tin nhóm")
                    } else {
                        crate::color::str_vi("STR_CONVERSATION_INFO", "Thông tin hội thoại")
                    })),
            )
            .child(
                div()
                    .id("info-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(12.))
                            .pt(px(24.))
                            .pb(px(16.))
                            .child({
                                let thread = conv.thread_id.clone();
                                div()
                                    .id("info-avatar")
                                    .when(is_group, |el| {
                                        el.cursor_pointer().on_click(cx.listener(
                                            move |this, _: &ClickEvent, _, cx| {
                                                this.pick_avatar_for(Some(thread.clone()), cx);
                                                cx.notify();
                                            },
                                        ))
                                    })
                                    .child(avatar)
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .text_size(px(18.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .child(conv.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .id("info-rename")
                                            .cursor_pointer()
                                            .text_color(rgb(muted))
                                            .on_click(cx.listener(
                                                move |this, _: &ClickEvent, _, cx| {
                                                    this.info_action(
                                                        id,
                                                        if is_group {
                                                            InfoAct::Rename
                                                        } else {
                                                            InfoAct::Alias
                                                        },
                                                        cx,
                                                    );
                                                    cx.notify();
                                                },
                                            ))
                                            .child(fonts::icon_el(g::EDIT, "✎", 18.)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_center()
                                    .gap(px(1.))
                                    .mt(px(4.))
                                    .child(action(
                                        self,
                                        "info-mute",
                                        if muted_on { g::UNMUTE } else { g::MUTE },
                                        if muted_on {
                                            "Bật thông\nbáo"
                                        } else {
                                            "Tắt thông\nbáo"
                                        },
                                        InfoAct::Mute,
                                        cx,
                                    ))
                                    .child(action(
                                        self,
                                        "info-pin",
                                        if pinned_on { g::UNPIN } else { g::PIN },
                                        if pinned_on {
                                            "Bỏ ghim\nhội thoại"
                                        } else {
                                            "Ghim hội\nthoại"
                                        },
                                        InfoAct::Pin,
                                        cx,
                                    ))
                                    .when(is_group, |el| {
                                        el.child(action(
                                            self,
                                            "info-add",
                                            g::ADD,
                                            "Thêm\nthành viên",
                                            InfoAct::AddMember,
                                            cx,
                                        ))
                                        .child(action(
                                            self,
                                            "info-manage",
                                            g::SETTING,
                                            "Quản lý\nnhóm",
                                            InfoAct::Manage,
                                            cx,
                                        ))
                                    })
                                    .when(!is_group, |el| {
                                        el.child(action(
                                            self,
                                            "info-create",
                                            g::ADD,
                                            "Tạo nhóm\ntrò chuyện",
                                            InfoAct::CreateGroup,
                                            cx,
                                        ))
                                    }),
                            ),
                    )
                    .child(gap())
                    .when(is_group, |el| {
                        let pending_label = crate::color::str_vi(
                            "STR_MENU_MEMBERSHIP_APPROVAL",
                            " Có $0$ yêu cầu tham gia nhóm",
                        )
                        .replace("$0$", &pending_n.to_string());
                        el.child(section(crate::color::str_vi(
                            "STR_MENU_GROUP_MEMBER",
                            "Thành viên nhóm",
                        )))
                        .child(line(
                            "info-members".into(),
                            g::MEMBERS,
                            crate::i18n::members(members),
                            Some(InfoAct::Members),
                            false,
                            cx,
                        ))
                        .when(!member_preview.is_empty(), |row| {
                            let ids = member_preview.clone();
                            row.child(
                                div()
                                    .id("info-member-preview")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.))
                                    .px(px(16.))
                                    .pb(px(12.))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.info_action(id, InfoAct::Members, cx);
                                        cx.notify();
                                    }))
                                    .children(ids.into_iter().enumerate().map(|(i, uid)| {
                                        let name = self
                                            .profiles
                                            .map
                                            .get(&uid)
                                            .map(|p| p.name.clone())
                                            .filter(|n| !n.is_empty())
                                            .unwrap_or_else(|| "Zalo".to_owned());
                                        let person = crate::dialogs::Person {
                                            uid,
                                            name,
                                            avatar: String::new(),
                                            note: String::new(),
                                        };
                                        div()
                                            .id(("info-mem-ava", i as u64))
                                            .child(self.dialog_avatar(&person, 32.))
                                    })),
                            )
                        })
                        .when(pending_n > 0, |row| {
                            row.child(
                                div()
                                    .id("info-pending")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.))
                                    .flex_none()
                                    .px(px(16.))
                                    .h(px(40.))
                                    .cursor_pointer()
                                    .hover(|st| {
                                        st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4")))
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.info_action(id, InfoAct::Pending, cx);
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .size(px(8.))
                                            .rounded(px(4.))
                                            .bg(rgb(token_rgb("--R60", "#c31818"))),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .text_size(px(14.))
                                            .text_color(rgb(token_rgb(
                                                "--text-information",
                                                "#005ae0",
                                            )))
                                            .truncate()
                                            .child(pending_label.trim().to_owned()),
                                    ),
                            )
                        })
                        .child(gap())
                        .child(section(crate::color::str_vi(
                            "STR_GROUP_BOARD",
                            "Bảng tin nhóm",
                        )))
                        .child(line(
                            "info-reminders".into(),
                            g::CLOCK,
                            crate::color::str_vi("STR_REMINDER_BOARD", "Danh sách nhắc hẹn")
                                .trim()
                                .to_owned(),
                            Some(InfoAct::Board(crate::board::BoardTab::Reminders)),
                            false,
                            cx,
                        ))
                        .child(line(
                            "info-notes".into(),
                            g::NOTE,
                            crate::color::str_vi("STR_NOTE_PIN_POLL", "Ghi chú, ghim, bình chọn")
                                .trim()
                                .to_owned(),
                            Some(InfoAct::Board(crate::board::BoardTab::All)),
                            false,
                            cx,
                        ))
                        .child(gap())
                    })
                    .when(!is_group, |el| {
                        el.child(section("Bảng tin"))
                            .child(line(
                                "info-reminders-1v1".into(),
                                g::CLOCK,
                                crate::color::str_vi("STR_REMINDER_BOARD", "Danh sách nhắc hẹn")
                                    .trim()
                                    .to_owned(),
                                Some(InfoAct::Board(crate::board::BoardTab::Reminders)),
                                false,
                                cx,
                            ))
                            .child(gap())
                    })
                    .child(section(crate::color::str_vi(
                        "STR_MENU_PHOTO_VIDEO",
                        "Ảnh/Video",
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(4.))
                            .px(px(16.))
                            .pb(px(12.))
                            .when(!use_cloud_photos && photos.is_empty(), |el| {
                                el.child(
                                    div().text_size(px(13.)).text_color(rgb(muted)).child(
                                        "Chưa có Ảnh/Video được chia sẻ trong hội thoại này",
                                    ),
                                )
                            })
                            .when(!use_cloud_photos, |el| {
                                el.children(photos.into_iter().map(|m| {
                                    let open = m.clone();
                                    let thumb = self.media_bitmaps.get(&m.id).cloned();
                                    let cell = photo_cell;
                                    div()
                                        .id(("info-photo", m.id))
                                        .size(px(cell))
                                        .rounded(px(4.))
                                        .overflow_hidden()
                                        .bg(rgb(subtle))
                                        .cursor_pointer()
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, window, cx| {
                                                this.open_viewer(open.clone(), window, cx);
                                                cx.notify();
                                            },
                                        ))
                                        .when_some(thumb.clone(), |el, t| {
                                            el.child(
                                                img(t)
                                                    .size(px(cell))
                                                    .object_fit(gpui::ObjectFit::Cover),
                                            )
                                        })
                                        .when(thumb.is_none(), |el| {
                                            el.flex()
                                                .items_center()
                                                .justify_center()
                                                .text_color(rgb(muted))
                                                .child(fonts::icon_el(fonts::ic::TB_PHOTO, "", 24.))
                                        })
                                }))
                            })
                            .when(use_cloud_photos, |el| {
                                el.children(cloud_photos.into_iter().enumerate().map(
                                    |(i, item)| {
                                        let thumb = self.store_thumb(&item.id);
                                        let open = item.clone();
                                        let thread = store_thread.clone();
                                        let cell = photo_cell;
                                        div()
                                            .id(("info-cloud-photo", i as u64))
                                            .size(px(cell))
                                            .rounded(px(4.))
                                            .overflow_hidden()
                                            .bg(rgb(subtle))
                                            .cursor_pointer()
                                            .on_click(cx.listener(
                                                move |this, _: &ClickEvent, window, cx| {
                                                    this.open_store_item(
                                                        &open, id, &thread, window, cx,
                                                    );
                                                    cx.notify();
                                                },
                                            ))
                                            .when_some(thumb, |el, t| {
                                                el.child(
                                                    img(t)
                                                        .size(px(cell))
                                                        .object_fit(gpui::ObjectFit::Cover),
                                                )
                                            })
                                            .when(self.store_thumb(&item.id).is_none(), |el| {
                                                el.flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .text_color(rgb(muted))
                                                    .child(fonts::icon_el(
                                                        fonts::ic::TB_PHOTO,
                                                        "",
                                                        24.,
                                                    ))
                                            })
                                    },
                                ))
                            }),
                    )
                    .child(gap())
                    .child(section(crate::color::str_vi("STR_FILE", "File")))
                    .when(!use_cloud_files && files.is_empty(), |el| {
                        el.child(
                            div()
                                .px(px(16.))
                                .pb(px(12.))
                                .text_size(px(13.))
                                .text_color(rgb(muted))
                                .child("Chưa có File được chia sẻ trong hội thoại này"),
                        )
                    })
                    .when(!use_cloud_files, |el| {
                        el.children(files.into_iter().map(|m| {
                            let size = if m.file_size > 0 {
                                format!(" · {}", crate::media::format_file_size(m.file_size))
                            } else {
                                String::new()
                            };
                            line(
                                format!("info-file-{}", m.id),
                                g::FILE,
                                format!("{}{size}", m.file_name),
                                None,
                                false,
                                cx,
                            )
                        }))
                    })
                    .when(use_cloud_files, |el| {
                        el.children(cloud_files.into_iter().enumerate().map(|(i, item)| {
                            let size = if item.file_size > 0 {
                                format!(" · {}", crate::media::format_file_size(item.file_size))
                            } else {
                                String::new()
                            };
                            let open = item.clone();
                            let thread = store_thread.clone();
                            let label = format!("{}{size}", item.title);
                            div()
                                .id(("info-cloud-file", i as u64))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(12.))
                                .flex_none()
                                .px(px(16.))
                                .h(px(48.))
                                .cursor_pointer()
                                .text_size(px(15.))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.open_store_item(&open, id, &thread, window, cx);
                                    cx.notify();
                                }))
                                .child(fonts::icon_el(g::FILE, "•", 20.))
                                .child(div().flex_1().min_w(px(0.)).truncate().child(label))
                        }))
                    })
                    .child(gap())
                    .child(section(crate::color::str_vi("STR_LINK", "Link")))
                    .when(!use_cloud_links && links.is_empty(), |el| {
                        el.child(
                            div()
                                .px(px(16.))
                                .pb(px(12.))
                                .text_size(px(13.))
                                .text_color(rgb(muted))
                                .child("Chưa có Link được chia sẻ trong hội thoại này"),
                        )
                    })
                    .when(!use_cloud_links, |el| {
                        el.children(links.into_iter().enumerate().map(|(i, (url, _))| {
                            let open = url.clone();
                            div()
                                .id(("info-link", i as u64))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(12.))
                                .flex_none()
                                .px(px(16.))
                                .h(px(44.))
                                .cursor_pointer()
                                .text_size(px(13.))
                                .text_color(rgb(token_rgb("--B55", "#0068ff")))
                                .on_click(
                                    cx.listener(move |_, _: &ClickEvent, _, cx| cx.open_url(&open)),
                                )
                                .child(fonts::icon_el(g::LINK, "🔗", 20.))
                                .child(div().flex_1().min_w(px(0.)).truncate().child(url))
                        }))
                    })
                    .when(use_cloud_links, |el| {
                        el.children(cloud_links.into_iter().enumerate().map(|(i, item)| {
                            let open = item.clone();
                            let thread = store_thread.clone();
                            let label = if item.title.is_empty() {
                                item.url.clone()
                            } else {
                                item.title.clone()
                            };
                            div()
                                .id(("info-cloud-link", i as u64))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(12.))
                                .flex_none()
                                .px(px(16.))
                                .h(px(44.))
                                .cursor_pointer()
                                .text_size(px(13.))
                                .text_color(rgb(token_rgb("--B55", "#0068ff")))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.open_store_item(&open, id, &thread, window, cx);
                                    cx.notify();
                                }))
                                .child(fonts::icon_el(g::LINK, "🔗", 20.))
                                .child(div().flex_1().min_w(px(0.)).truncate().child(label))
                        }))
                    })
                    .child(gap())
                    .when_some(e2ee_line, |el, text| {
                        el.child(
                            div()
                                .px(px(16.))
                                .pb(px(10.))
                                .text_size(px(13.))
                                .line_height(px(18.))
                                .text_color(rgb(muted))
                                .child(crate::i18n::t(text)),
                        )
                    })
                    .child(section(crate::color::str_vi(
                        "STR_SETTINGS_PRIVACY_2",
                        "Thiết lập bảo mật",
                    )))
                    .child({
                        let hidden_on = self.hidden.contains(&id);
                        let hide_label = crate::color::str_vi(
                            "STR_ENTRY_POINT_HIDE_CONVERSATION",
                            "Ẩn trò chuyện",
                        )
                        .trim()
                        .to_owned();
                        div()
                            .id("info-hide")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .flex_none()
                            .px(px(16.))
                            .h(px(48.))
                            .text_size(px(15.))
                            .cursor_pointer()
                            .hover(|st| {
                                st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4")))
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.info_action(id, InfoAct::Hide, cx);
                                cx.notify();
                            }))
                            .child(fonts::icon_el(g::HIDE, "•", 20.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .child(crate::i18n::t(&hide_label)),
                            )
                            .child(switch_pill(hidden_on))
                    })
                    .child(line(
                        "info-report".into(),
                        g::WARN,
                        crate::color::str_vi("STR_REPORT_USER", "Báo xấu").to_owned(),
                        Some(InfoAct::Report),
                        false,
                        cx,
                    ))
                    .child(gap())
                    .child(line(
                        "info-clear".into(),
                        g::TRASH,
                        crate::color::str_vi("STR_DELETE_CHAT", "Xoá lịch sử trò chuyện")
                            .to_owned(),
                        Some(InfoAct::Clear),
                        true,
                        cx,
                    ))
                    .when(is_group, |el| {
                        el.child(line(
                            "info-leave".into(),
                            g::LEAVE,
                            if leave_armed {
                                "Bấm lần nữa để rời nhóm".into()
                            } else {
                                crate::color::str_vi("STR_LEAVEGROUP_MENU", "Rời nhóm").to_owned()
                            },
                            Some(InfoAct::Leave),
                            true,
                            cx,
                        ))
                        .when(leave_armed, |row| {
                            let silent = self.leave_silent;
                            let caption = crate::color::str_vi(
                                "STR_LEAVE_GROUP_SILENTLY_SWITCH_DESC",
                                "Chỉ trưởng/phó nhóm biết bạn rời nhóm.",
                            )
                            .trim()
                            .to_owned();
                            row.child(
                                div()
                                    .id("info-leave-silent")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(10.))
                                    .px(px(16.))
                                    .pb(px(12.))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.info_action(id, InfoAct::LeaveSilent, cx);
                                        cx.notify();
                                    }))
                                    .child(check_box(silent))
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(13.))
                                            .text_color(rgb(muted))
                                            .child(caption),
                                    ),
                            )
                        })
                    }),
            )
            .into_any_element()
    }

    fn info_action(&mut self, id: u64, act: InfoAct, cx: &mut Context<Self>) {
        use crate::bridge::LiveAction;
        let Some(conv) = self.conv(id).cloned() else {
            return;
        };
        match act {
            InfoAct::Members => self.open_members(id, cx),
            InfoAct::Rename => self.open_rename(id, cx),
            InfoAct::Mute => {
                let mute = !self.muted.contains(&id);
                if mute {
                    self.muted.insert(id);
                } else {
                    self.muted.remove(&id);
                }
                self.save_conv_flags();
                self.maybe_live(LiveAction::Mute {
                    thread_id: conv.thread_id,
                    is_group: conv.is_group,
                    mute,
                    duration: -1,
                });
            }
            InfoAct::Pin => {
                let pin = !self.pinned.contains(&id);
                if pin {
                    self.pinned.insert(id);
                } else {
                    self.pinned.remove(&id);
                }
                self.save_conv_flags();
                self.maybe_live(LiveAction::Pin {
                    thread_id: conv.thread_id,
                    pin,
                });
            }
            InfoAct::AddMember => {
                let grid = conv
                    .thread_id
                    .strip_prefix("g_")
                    .unwrap_or(&conv.thread_id)
                    .to_owned();
                self.open_group_picker(Some((grid, conv.name)), cx);
            }
            InfoAct::CreateGroup => self.open_group_picker(None, cx),
            InfoAct::Manage => self.open_group_manage(id, cx),
            InfoAct::Pending => {
                self.open_group_manage_tab(id, crate::account::GroupManageTab::Pending, cx)
            }
            InfoAct::Hide => {
                if self.hidden.contains(&id) {
                    self.unhide_conv(id, cx);
                } else {
                    self.hide_conv(id, cx);
                }
            }
            InfoAct::Clear => {
                self.messages.insert(id, Vec::new());
                self.set_note("Đã xóa lịch sử trên máy này");
            }
            InfoAct::Leave => {
                if self.leave_armed != Some(id) {
                    self.leave_armed = Some(id);
                    self.leave_silent = false;
                    return;
                }
                let silent = self.leave_silent;
                self.leave_group_now(id, silent, cx);
            }
            InfoAct::LeaveSilent => {
                if self.leave_armed == Some(id) {
                    self.leave_silent = !self.leave_silent;
                }
            }
            InfoAct::Report => self.open_report(id, cx),
            InfoAct::Alias => {
                self.open_alias(conv.thread_id, conv.name, cx);
            }
            InfoAct::Board(tab) => {
                self.open_board_dialog(id, tab, cx);
            }
        }
    }

    /// Sends `/api/group/leave` and hides the group row locally on success.
    /// `silent` is Electron's "Leave group silently" (`silent: 1`).
    pub(crate) fn leave_group_now(&mut self, id: u64, silent: bool, cx: &mut Context<Self>) {
        let Some(conv) = self.conv(id).cloned() else {
            return;
        };
        self.leave_armed = None;
        self.leave_silent = false;
        let grid = conv.thread_id;
        if let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) {
            cx.spawn(async move |this, cx| {
                let r = cx
                    .background_executor()
                    .spawn(async move {
                        crate::bridge::block_api(&dir, |api| async move {
                            api.leave_group(&grid, silent)
                                .await
                                .map_err(|e| e.to_string())
                        })
                    })
                    .await;
                let _ = this.update(cx, |view, cx| {
                    match r {
                        Ok(_) => {
                            view.removed.insert(id);
                            view.save_conv_flags();
                            view.selected = None;
                            view.info_open = false;
                            view.set_note("Đã rời nhóm.");
                        }
                        Err(e) => view.set_note(format!("Rời nhóm thất bại: {e}")),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// One pending-member fetch per group while the info column is open.
    ///
    /// Electron only calls pending for the owner or an admin. A regular
    /// member gets Zalo 114, which this used to swallow as a zero count.
    pub(crate) fn warm_group_pending(&mut self, conv_id: u64, cx: &mut Context<Self>) {
        if cfg!(test) || !self.has_session {
            return;
        }
        if self.group_pending_busy.contains(&conv_id) || self.group_pending_n.contains_key(&conv_id)
        {
            return;
        }
        let Some(conv) = self
            .conv(conv_id)
            .cloned()
            .filter(|c| c.is_group && crate::data::live_allowed(&c.thread_id))
        else {
            return;
        };
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let mine = self.self_member_ids();
        if let Some(view) = self.members_view.as_ref().filter(|m| m.conv == conv_id) {
            if !view.creator.is_empty()
                && !may_list_group_pending(&mine, &view.creator, &view.admins)
            {
                self.group_pending_n.insert(conv_id, 0);
                return;
            }
        }
        self.group_pending_busy.insert(conv_id);
        let grid = conv.thread_id;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let info = api
                            .get_group_info_v2(&grid, 1)
                            .await
                            .map_err(|e| e.to_string())?;
                        let (_, creator, admins) = members_from(&info);
                        if !may_list_group_pending(&mine, &creator, &admins) {
                            return Ok(0);
                        }
                        api.get_group_pending_members(&grid)
                            .await
                            .map(|v| crate::account::group_pending_from(&v).len())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.group_pending_busy.remove(&conv_id);
                // A failed list stays at zero so render does not refetch every frame.
                // Opening "Member Approval" overwrites this with the real count.
                view.group_pending_n.insert(conv_id, res.unwrap_or(0));
                cx.notify();
            });
        })
        .detach();
    }

    /// Switch the info column to "Members" and fetch the member page.
    pub(crate) fn open_members(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(conv) = self.conv(id).cloned() else {
            return;
        };
        self.members_view = Some(MembersView {
            conv: id,
            loading: true,
            ..MembersView::default()
        });
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            let known: Vec<(String, String)> = self
                .members_of(&conv)
                .into_iter()
                .map(|n| (String::new(), n))
                .collect();
            if let Some(v) = self.members_view.as_mut() {
                v.loading = false;
                v.list = known;
                v.error = Some("Chưa đăng nhập — chỉ hiện người đã nhắn trong nhóm".into());
            }
            return;
        };
        let grid = conv.thread_id.clone();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.get_group_info_v2(&grid, 1)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let Some(v) = view.members_view.as_mut().filter(|v| v.conv == id) else {
                    return;
                };
                v.loading = false;
                match r {
                    Ok(value) => {
                        let (list, creator, admins) = members_from(&value);
                        v.creator = creator;
                        v.admins = admins;
                        v.list = list
                            .iter()
                            .map(|(u, n, _)| (u.clone(), n.clone()))
                            .collect();
                        // Unknown people: keep name + avatar so the avatar loader picks them up.
                        for (uid, name, avatar) in list {
                            view.profiles.map.entry(uid).or_insert_with(|| {
                                crate::profiles::Profile {
                                    name,
                                    avatar,
                                    ..crate::profiles::Profile::default()
                                }
                            });
                        }
                        view.avatar_all = true;
                        view.avatars_queued = true;
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads `/api/group/getlg/e2ee` once. Preview and tests do not call the network.
    pub(crate) fn warm_e2ee_groups(&mut self, cx: &mut Context<Self>) {
        if self.e2ee_groups.is_some()
            || self.e2ee_busy
            || self.e2ee_failed
            || cfg!(test)
            || !self.has_session
        {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        self.e2ee_busy = true;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.get_e2ee_group_list()
                            .await
                            .map(|value| zalo_client::features::e2ee_group_ids(&value))
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.e2ee_busy = false;
                match res {
                    Ok(ids) => view.e2ee_groups = Some(ids),
                    Err(_) => view.e2ee_failed = true,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the product catalog once. Does not create products and does not run in preview.
    pub(crate) fn warm_catalog(&mut self, cx: &mut Context<Self>) {
        if self.catalog_loaded || self.catalog_busy {
            return;
        }
        if cfg!(test) || !self.has_session {
            self.catalog_loaded = true;
            self.catalog_note = "Chưa đăng nhập. Danh sách lấy từ /api/prodcatalog/catalog/list khi có phiên. Máy này không tạo sản phẩm.".to_owned();
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            self.catalog_loaded = true;
            self.catalog_note = "Chưa đăng nhập. Danh sách lấy từ /api/prodcatalog/catalog/list khi có phiên. Máy này không tạo sản phẩm.".to_owned();
            return;
        };
        self.catalog_busy = true;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.list_catalogs().await.map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.catalog_busy = false;
                view.catalog_loaded = true;
                match res {
                    Ok(rows) => {
                        view.catalog_rows = rows;
                        view.catalog_note.clear();
                    }
                    Err(_) => {
                        view.catalog_note = "Chưa tải được danh mục.".to_owned();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Kicks a member from the group via `/api/group/kickout` and refreshes local lists.
    pub(crate) fn kick_group_member(
        &mut self,
        conv_id: u64,
        uid: String,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(conv) = self.conv(conv_id).cloned().filter(|c| c.is_group) else {
            return;
        };
        if cfg!(test) || !self.has_session || !crate::data::live_allowed(&conv.thread_id) {
            self.set_note("Chưa đăng nhập — không thể xóa thành viên");
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if let Some(v) = self.members_view.as_mut().filter(|v| v.conv == conv_id) {
            if !v.kicking.insert(uid.clone()) {
                return;
            }
        }
        let grid = conv.thread_id.clone();
        let target_uid = uid.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.kick_group_members(&grid, &[target_uid])
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(v) = view.members_view.as_mut().filter(|v| v.conv == conv_id) {
                    v.kicking.remove(&uid);
                    if res.is_ok() {
                        v.list.retain(|(id, _)| id != &uid);
                        v.admins.retain(|id| id != &uid);
                    }
                }
                match res {
                    Ok(_) => {
                        if let Some(profile) = view.profiles.map.get_mut(&conv.thread_id) {
                            profile.member_ids.retain(|id| id != &uid);
                            profile.members = profile.members.saturating_sub(1);
                        }
                        view.set_note(format!("Đã mời {name} ra khỏi nhóm"));
                    }
                    Err(e) => view.set_note(format!("Không thể xóa thành viên: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Promotes/demotes group co-admins or transfers group creator ownership.
    pub(crate) fn set_group_member_role(
        &mut self,
        conv_id: u64,
        uid: String,
        name: String,
        action: GroupRoleAction,
        cx: &mut Context<Self>,
    ) {
        let Some(conv) = self.conv(conv_id).cloned().filter(|c| c.is_group) else {
            return;
        };
        if cfg!(test) || !self.has_session || !crate::data::live_allowed(&conv.thread_id) {
            self.set_note("Chưa đăng nhập — không thể cập nhật vai trò");
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if let Some(v) = self.members_view.as_mut().filter(|v| v.conv == conv_id) {
            if !v.kicking.insert(uid.clone()) {
                return;
            }
            v.owner_armed = None;
        }
        let grid = conv.thread_id.clone();
        let target_uid = uid.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        match action {
                            GroupRoleAction::PromoteAdmin => {
                                api.add_group_admins(&grid, &[target_uid]).await
                            }
                            GroupRoleAction::DemoteAdmin => {
                                api.remove_group_admins(&grid, &[target_uid]).await
                            }
                            GroupRoleAction::TransferOwner => {
                                api.change_group_owner(&grid, &target_uid).await
                            }
                        }
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(v) = view.members_view.as_mut().filter(|v| v.conv == conv_id) {
                    v.kicking.remove(&uid);
                    if res.is_ok() {
                        match action {
                            GroupRoleAction::PromoteAdmin => {
                                if !v.admins.contains(&uid) {
                                    v.admins.push(uid.clone());
                                }
                            }
                            GroupRoleAction::DemoteAdmin => {
                                v.admins.retain(|id| id != &uid);
                            }
                            GroupRoleAction::TransferOwner => {
                                v.creator = uid.clone();
                                v.admins.retain(|id| id != &uid);
                            }
                        }
                    }
                }
                if res.is_ok() {
                    if let Some(crate::dialogs::Dialog::GroupManage(d)) = view.dialog.as_mut() {
                        if d.conv_id == conv_id {
                            match action {
                                GroupRoleAction::PromoteAdmin => {
                                    if !d.admins.contains(&uid) {
                                        d.admins.push(uid.clone());
                                    }
                                }
                                GroupRoleAction::DemoteAdmin => d.admins.retain(|id| id != &uid),
                                GroupRoleAction::TransferOwner => {
                                    d.creator = uid.clone();
                                    d.admins.retain(|id| id != &uid);
                                }
                            }
                        }
                    }
                    let note = match action {
                        GroupRoleAction::PromoteAdmin => format!("Đã bổ nhiệm {name} làm phó nhóm"),
                        GroupRoleAction::DemoteAdmin => format!("Đã gỡ quyền phó nhóm của {name}"),
                        GroupRoleAction::TransferOwner => {
                            format!("Đã chuyển quyền trưởng nhóm cho {name}")
                        }
                    };
                    view.set_note(note);
                } else if let Err(e) = res {
                    view.set_note(format!("Không thể cập nhật vai trò: {e}"));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `#conversation-info` "Members": back, "Add Members", list with roles.
    fn members_panel(
        &self,
        conv: &ChatConv,
        view: &MembersView,
        corners: CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let id = conv.id;
        let me = self.uin.clone().unwrap_or_default();
        let mut rows = view.list.clone();
        // Creator, then admins, then everyone else (Zalo PC order).
        rows.sort_by_key(|(uid, _)| {
            if *uid == view.creator {
                0
            } else if view.admins.contains(uid) {
                1
            } else {
                2
            }
        });
        corners
            .apply(
                div()
                    .id("conversation-info")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w(px(INFO_PANEL_W))
                    .h_full()
                    .bg(rgb(paper))
                    .border_l_1()
                    .border_color(crate::color::border_subtle())
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
                    .border_color(crate::color::border_subtle())
                    .child(
                        div()
                            .id("members-back")
                            .size(px(32.))
                            .rounded(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.members_view = None;
                                cx.notify();
                            }))
                            .child(fonts::icon_el('\u{EA8E}', "‹", 20.)),
                    )
                    .child(div().flex_1().text_center().mr(px(32.)).text_size(px(18.)).font_family(FAMILY_SEMIBOLD).child("Thành viên")),
            )
            .child(
                div().flex_none().px(px(16.)).pt(px(16.)).child(
                    div()
                        .id("members-add")
                        .h(px(32.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .gap(px(8.))
                        .rounded(px(4.))
                        .bg(rgb(crate::color::token_rgb("--button-secondary-neutral-normal", "#e5e7eb")))
                        .hover(|st| st.bg(rgb(crate::color::token_rgb("--button-secondary-neutral-hover", "#d6d9de"))))
                        .cursor_pointer()
                        .text_size(px(14.))
                        .font_family(FAMILY_SEMIBOLD)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.info_action(id, InfoAct::AddMember, cx);
                            cx.notify();
                        }))
                        .child(fonts::icon_el(g::ADD, "+", 18.))
                        .child(crate::i18n::t("Thêm thành viên")),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(8.))
                    .text_size(px(14.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child(if view.loading {
                        "Danh sách thành viên".to_owned()
                    } else {
                        format!("Danh sách thành viên ({})", rows.len())
                    }),
            )
            .when_some(view.error.clone(), |el, e| {
                el.child(div().px(px(16.)).pb(px(8.)).text_size(px(13.)).text_color(rgb(muted)).child(e))
            })
            .when(view.loading, |el| {
                el.child(div().px(px(16.)).text_size(px(13.)).text_color(rgb(muted)).child("Đang tải…"))
            })
            .child({
                let mine = self.self_member_ids();
                let conv_id = conv.id;
                div()
                    .id("members-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .children(rows.into_iter().map(|(uid, name)| {
                        let role = if uid == view.creator {
                            Some("Trưởng nhóm")
                        } else if view.admins.contains(&uid) {
                            Some("Phó nhóm")
                        } else {
                            None
                        };
                        let is_self = (!me.is_empty() && uid == me) || mine.contains(&uid);
                        let shown = if is_self {
                            "Bạn".to_owned()
                        } else if name.is_empty() {
                            self.profiles.map.get(&uid).map(|p| p.name.clone()).unwrap_or_else(|| uid.clone())
                        } else {
                            name
                        };
                        let kickable = can_kick_member(&mine, &view.creator, &view.admins, &uid);
                        let role_manageable = can_manage_group_roles(&mine, &view.creator, &uid);
                        let is_target_admin = view.admins.contains(&uid);
                        let owner_confirm = view.owner_armed.as_deref() == Some(uid.as_str());
                        let busy = view.kicking.contains(&uid);
                        let person = crate::dialogs::Person { uid: uid.clone(), name: shown.clone(), avatar: String::new(), note: String::new() };
                        let (open, open_name, kick_uid, kick_name) = (uid.clone(), shown.clone(), uid.clone(), shown.clone());
                        let (role_uid, role_name, own_uid, own_name) = (uid.clone(), shown.clone(), uid.clone(), shown.clone());
                        div()
                            .id(gpui::SharedString::from(format!("member-{uid}")))
                            .flex()
                            .flex_none()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .h(px(56.))
                            .px(px(16.))
                            .cursor_pointer()
                            .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_chat_or_note(
                                    &open,
                                    &open_name,
                                    false,
                                    "Chưa có hội thoại 1-1 với người này",
                                    window,
                                    cx,
                                );
                                cx.notify();
                            }))
                            .child(self.dialog_avatar(&person, 40.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(div().text_size(px(15.)).truncate().child(shown))
                                    .when_some(role, |el, r| {
                                        el.child(div().flex().flex_row().items_center().gap(px(4.)).text_size(px(13.)).text_color(rgb(muted))
                                            .child(div().text_color(rgb(0xe8_a3_17)).child(fonts::icon_el('\u{EC35}', "", 14.)))
                                            .child(r))
                                    }),
                            )
                            .when(role_manageable || kickable, |el| {
                                let action_btn = |id: gpui::SharedString, label: &'static str, color: u32| {
                                    div()
                                        .id(id)
                                        .px(px(7.))
                                        .py(px(3.))
                                        .rounded(px(4.))
                                        .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
                                        .text_size(px(11.5))
                                        .text_color(rgb(color))
                                        .cursor_pointer()
                                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .child(label)
                                };
                                el.child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .flex_wrap()
                                        .justify_end()
                                        .gap(px(4.))
                                        .when(role_manageable && !busy, |row| {
                                            let next_role = if is_target_admin {
                                                GroupRoleAction::DemoteAdmin
                                            } else {
                                                GroupRoleAction::PromoteAdmin
                                            };
                                            let role_label = if is_target_admin { "Bỏ P.nhóm" } else { "+ Phó nhóm" };
                                            let own_label = if owner_confirm { "Xác nhận?" } else { "Trưởng nhóm" };
                                            row.child(
                                                action_btn(
                                                    gpui::SharedString::from(format!("role-{role_uid}")),
                                                    role_label,
                                                    token_rgb("--blue", "#0068ff"),
                                                )
                                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                                    cx.stop_propagation();
                                                    this.set_group_member_role(conv_id, role_uid.clone(), role_name.clone(), next_role, cx);
                                                })),
                                            )
                                            .child(
                                                action_btn(
                                                    gpui::SharedString::from(format!("own-{own_uid}")),
                                                    own_label,
                                                    if owner_confirm {
                                                        token_rgb("--R60", "#c31818")
                                                    } else {
                                                        muted
                                                    },
                                                )
                                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                                    cx.stop_propagation();
                                                    if owner_confirm {
                                                        this.set_group_member_role(conv_id, own_uid.clone(), own_name.clone(), GroupRoleAction::TransferOwner, cx);
                                                    } else if let Some(v) = this.members_view.as_mut().filter(|v| v.conv == conv_id) {
                                                        v.owner_armed = Some(own_uid.clone());
                                                        this.set_note("Bấm lại để xác nhận chuyển trưởng nhóm");
                                                        cx.notify();
                                                    }
                                                })),
                                            )
                                        })
                                        .when(kickable, |row| {
                                            row.child(
                                                action_btn(
                                                    gpui::SharedString::from(format!("kick-{kick_uid}")),
                                                    if busy { "Đang xóa…" } else { "Xóa" },
                                                    token_rgb("--R60", "#c31818"),
                                                )
                                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                                    cx.stop_propagation();
                                                    this.kick_group_member(conv_id, kick_uid.clone(), kick_name.clone(), cx);
                                                    cx.notify();
                                                })),
                                            )
                                        }),
                                )
                            })
                    }))
            })
    }

    pub(crate) fn members_of(&self, conv: &ChatConv) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(msgs) = self.messages.get(&conv.id) {
            for msg in msgs {
                if msg.from_me || msg.sender.is_empty() {
                    continue;
                }
                if !names.iter().any(|n| n == &msg.sender) {
                    names.push(msg.sender.clone());
                }
            }
        }
        if names.is_empty() {
            names.push(conv.name.clone());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_reply_gives_roles() {
        let v = serde_json::json!({"gridInfoMap": {"9": {
            "creatorId": "1", "adminIds": ["2"],
            "currentMems": [{"id": "1", "dName": "A"}, {"id": "2", "dName": "B"}, {"id": "3", "dName": "C", "avatar": "https://x/c.jpg"}]
        }}});
        let (list, creator, admins) = members_from(&v);
        assert_eq!(list.len(), 3);
        assert_eq!(list[2].2, "https://x/c.jpg");
        assert_eq!(creator, "1");
        assert_eq!(admins, ["2"]);
    }

    #[test]
    fn info_panel_matches_list_width() {
        assert_eq!(INFO_PANEL_W, 344.0);
        assert_eq!(INFO_PANEL_W, zalo_design::CONV_LIST_INNER_W);
        // 64 + 344 + 420 + 344 = 1172. The default 1200 px window docks.
        assert!(info_docks(1200.0, false));
        assert!(!info_docks(1000.0, false));
        assert!(info_docks(900.0, true));
        assert!(!info_docks(800.0, true));
    }

    #[test]
    fn role_permissions_and_transfer_checks() {
        let me: std::collections::HashSet<String> = ["1".into()].into_iter().collect();
        assert!(can_manage_group_roles(&me, "1", "2"));
        assert!(!can_manage_group_roles(&me, "1", "1"));
        assert!(!can_manage_group_roles(&me, "2", "3"));
        assert!(may_list_group_pending(&me, "1", &[]));
        let admin: std::collections::HashSet<String> = ["2".into()].into_iter().collect();
        assert!(may_list_group_pending(&admin, "1", &["2".into()]));
        let member: std::collections::HashSet<String> = ["3".into()].into_iter().collect();
        assert!(!may_list_group_pending(&member, "1", &["2".into()]));
        assert!(!may_list_group_pending(&member, "", &[]));
    }
}
