//! "Group Info" / "Conversation Info" column, after the live Zalo PC
//! panel: 344 wide, centered title, avatar + name, four round actions,
//! members (count, faces, join link), group board, uniform photo grid,
//! file and link rows with a date, then security (vanish, block sharing,
//! hide, report, delete, leave). "Xem tất cả" expands inside this column.
//! Sections are separated by 8 px `--layer-background-subtle`.

use gpui::{
    div, img, prelude::*, px, rgb, ClickEvent, ClipboardItem, Context, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement, Styled,
};

use crate::color::token_rgb;
use crate::data::ChatConv;
use crate::fonts::{self, ic, FAMILY_SEMIBOLD};
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
    pub const LINK: char = '\u{EC43}';
    pub const HIDE: char = '\u{EB11}';
    pub const TRASH: char = '\u{EDCF}';
    pub const LEAVE: char = '\u{EBC4}';
    pub const WARN: char = '\u{EE04}';
    pub const COPY: char = '\u{EAC5}';
    pub const SHARE: char = '\u{ED29}';
}

/// `https://zalo.me/g/…` → `zalo.me/g/…`.
fn short_join_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed)
        .to_owned()
}

/// Host without scheme or a leading `www.`.
fn link_host_label(url: &str) -> String {
    let host = crate::media::host_of(url);
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

/// `sendDttm` as `dd/mm/yyyy`. Seconds are scaled up to milliseconds.
fn shelf_ymd(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let ms = if ms < 1_000_000_000_000 {
        ms.saturating_mul(1000)
    } else {
        ms
    };
    let (year, month, day) = crate::data::vn_ymd(ms);
    format!("{day:02}/{month:02}/{year}")
}

fn link_row_title(text: &str, file_name: &str, url: &str) -> String {
    let named = file_name.trim();
    if !named.is_empty() && named != url && !named.starts_with("http") {
        return named.to_owned();
    }
    let body = text.trim();
    if !body.is_empty()
        && body != url
        && !body.starts_with("http://")
        && !body.starts_with("https://")
    {
        return body.to_owned();
    }
    let short = short_join_url(url);
    if short.is_empty() {
        url.to_owned()
    } else {
        short
    }
}

fn file_row_name(file_name: &str, text: &str) -> String {
    let name = file_name.trim();
    if !name.is_empty() {
        return name.to_owned();
    }
    let text = text.trim();
    text.strip_prefix("[File]")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(text)
        .to_owned()
}

fn info_section_head(title: &'static str) -> impl IntoElement {
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
}

fn info_empty(text: &'static str, muted: u32) -> impl IntoElement {
    div()
        .px(px(16.))
        .pb(px(12.))
        .text_size(px(13.))
        .text_color(rgb(muted))
        .child(text)
}

fn file_mark(name: &str) -> gpui::AnyElement {
    let ext = crate::media::file_ext_label(name);
    let color = crate::media::file_ext_color(name);
    let image = matches!(
        ext.as_str(),
        "PNG" | "JPG" | "JPEG" | "GIF" | "WEBP" | "BMP" | "HEIC"
    );
    div()
        .flex_none()
        .w(px(36.))
        .h(px(44.))
        .rounded(px(4.))
        .bg(rgb(color))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(0xffffff))
        .text_size(px(11.))
        .font_family(FAMILY_SEMIBOLD)
        .child(if image {
            fonts::icon_el(fonts::ic::TB_PHOTO, "", 18.).into_any_element()
        } else {
            div().child(ext).into_any_element()
        })
        .into_any_element()
}

fn link_mark() -> gpui::AnyElement {
    div()
        .flex_none()
        .size(px(36.))
        .rounded(px(8.))
        .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(token_rgb("--text-secondary", "#5a6981")))
        .child(fonts::icon_el(g::LINK, "🔗", 18.))
        .into_any_element()
}

fn square_photo(
    id: impl Into<gpui::ElementId>,
    cell: f32,
    thumb: Option<std::sync::Arc<gpui::RenderImage>>,
    subtle: u32,
    muted: u32,
    cx: &mut Context<ShellView>,
    click: impl Fn(&mut ShellView, &ClickEvent, &mut gpui::Window, &mut Context<ShellView>) + 'static,
) -> gpui::AnyElement {
    let missing = thumb.is_none();
    div()
        .id(id)
        .relative()
        .flex_none()
        .w(px(cell))
        .h(px(cell))
        .min_w(px(cell))
        .max_w(px(cell))
        .min_h(px(cell))
        .max_h(px(cell))
        .rounded(px(4.))
        .overflow_hidden()
        .bg(rgb(subtle))
        .cursor_pointer()
        .on_click(cx.listener(click))
        .when_some(thumb, |el, image| {
            el.child(
                img(image)
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(cell))
                    .h(px(cell))
                    .object_fit(gpui::ObjectFit::Cover),
            )
        })
        .when(missing, |el| {
            el.flex()
                .items_center()
                .justify_center()
                .text_color(rgb(muted))
                .child(fonts::icon_el(fonts::ic::TB_PHOTO, "", 22.))
        })
        .into_any_element()
}

fn dated_row(
    id: impl Into<gpui::ElementId>,
    mark: impl IntoElement,
    title: String,
    meta: String,
    checked: bool,
    date: String,
    cx: &mut Context<ShellView>,
    click: impl Fn(&mut ShellView, &ClickEvent, &mut gpui::Window, &mut Context<ShellView>) + 'static,
) -> gpui::AnyElement {
    let ink = token_rgb("--text-primary", "#081b3a");
    let muted = token_rgb("--text-secondary", "#5a6981");
    let show_date = !date.is_empty();
    let show_meta = !meta.is_empty();
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.))
        .flex_none()
        .px(px(16.))
        .py(px(8.))
        .min_h(px(56.))
        .cursor_pointer()
        .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
        .on_click(cx.listener(click))
        .child(mark)
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
                        .text_color(rgb(ink))
                        .truncate()
                        .child(title),
                )
                .when(show_meta, |el| {
                    el.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.))
                            .min_w(px(0.))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .text_size(px(12.))
                                    .text_color(rgb(muted))
                                    .truncate()
                                    .child(meta),
                            )
                            .when(checked, |row| {
                                row.child(
                                    div()
                                        .flex_none()
                                        .text_color(rgb(0x15_a8_5f))
                                        .child(fonts::icon_el('\u{EC54}', "✓", 12.)),
                                )
                            }),
                    )
                }),
        )
        .when(show_date, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(rgb(muted))
                    .child(date),
            )
        })
        .into_any_element()
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
    AutoDelete,
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
    let track = if on {
        crate::color::accent_bg()
    } else {
        crate::color::switch_off_bg()
    };
    let knob = crate::color::switch_knob_fg(on);
    div()
        .flex_none()
        .w(px(36.))
        .h(px(20.))
        .px(px(2.))
        .rounded(px(10.))
        .flex()
        .items_center()
        .bg(rgb(track))
        .when(on, |el| el.justify_end())
        .when(!on, |el| el.justify_start())
        .child(div().size(px(16.)).rounded(px(8.)).bg(rgb(knob)))
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
            crate::color::accent_bg()
        } else {
            token_rgb("--layer-background", "#ffffff")
        }))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .text_color(rgb(crate::color::on_accent_fg()))
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
        if self.info_shelf.is_some_and(|(cid, _)| cid == conv.id) {
            let kind = self.info_shelf.map(|(_, kind)| kind).unwrap_or(0);
            return self
                .media_shelf_panel(&conv, kind, corners, cx)
                .into_any_element();
        }
        let is_group = conv.is_group;
        let id = conv.id;
        let members = self
            .members_known(&conv.thread_id)
            .map(|n| n as usize)
            .unwrap_or_else(|| self.members_of(&conv).len().max(1));
        let pending_n = self.group_pending_n.get(&id).copied().unwrap_or(0);
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
                        0x26_26_26
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
        let own = self.shown_photo(&conv.thread_id);
        let faces = if is_group && own.is_none() && !cloud {
            self.group_collage_faces(&conv.thread_id)
        } else {
            Vec::new()
        };
        let paint = crate::sidebar::avatar_paint(is_group, own.is_some(), cloud, faces.len());
        let avatar = match paint {
            crate::sidebar::AvatarPaint::Photo => div()
                .size(px(56.))
                .rounded(px(28.))
                .overflow_hidden()
                .border_1()
                .border_color(crate::color::border_subtle())
                .bg(rgb(crate::color::token_rgb(
                    "--layer-background",
                    "#ffffff",
                )))
                .children(own.map(|i| {
                    img(i)
                        .size_full()
                        .rounded(px(28.))
                        .object_fit(gpui::ObjectFit::Cover)
                }))
                .into_any_element(),
            crate::sidebar::AvatarPaint::Cloud => div()
                .size(px(56.))
                .rounded(px(28.))
                .bg(rgb(crate::color::neutral_avatar_bg()))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0xffffff))
                .child(fonts::icon_el(ic::DOCS_ON, "☁", 38.))
                .into_any_element(),
            crate::sidebar::AvatarPaint::Collage => {
                crate::sidebar::group_collage(&faces, 56.).into_any_element()
            }
            crate::sidebar::AvatarPaint::GroupMark => {
                crate::sidebar::group_mark(56.).into_any_element()
            }
            crate::sidebar::AvatarPaint::Letter => crate::sidebar::letter_disc(
                &crate::sidebar::avatar_initials(&conv.name),
                if is_group {
                    crate::color::neutral_avatar_bg()
                } else {
                    crate::sidebar::avatar_color(&conv.thread_id)
                },
                56.,
            )
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
                                            .child(if cloud {
                                                "My Documents".to_owned()
                                            } else {
                                                conv.name.clone()
                                            }),
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
                        .when_some(
                            self.info_join
                                .get(&id)
                                .cloned()
                                .filter(|url| !url.is_empty()),
                            |row, url| row.child(self.join_link_row(url, cx)),
                        )
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
                    .child(self.shared_media_block(&conv, cx))
                    .child(section(crate::color::str_vi(
                        "STR_SETTINGS_PRIVACY_2",
                        "Thiết lập bảo mật",
                    )))
                    .child(self.info_stack_row(
                        "info-vanish",
                        g::CLOCK,
                        crate::color::str_vi("STR_VANISH_MSGS", "Tin nhắn tự xóa"),
                        &self.thread_ttl_label(&conv.thread_id),
                        id,
                        InfoAct::AutoDelete,
                        cx,
                    ))
                    .when(is_group, |el| {
                        el.child(self.info_stack_row(
                            "info-block-share",
                            g::SHARE,
                            crate::color::str_vi("STR_BLOCK_SHARING_STATUS_TITLE", "Chặn chia sẻ"),
                            crate::color::str_vi(
                                "STR_BLOCK_SHARING_STATUS_DESC",
                                "Bảo vệ nội dung trò chuyện",
                            ),
                            id,
                            InfoAct::Manage,
                            cx,
                        ))
                    })
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
            InfoAct::AutoDelete => self.open_auto_delete(id, cx),
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

    /// Forces a fresh reload of the product catalog from `/api/prodcatalog/catalog/list`.
    pub(crate) fn reload_catalog(&mut self, cx: &mut Context<Self>) {
        self.catalog_loaded = false;
        self.warm_catalog(cx);
    }

    /// Reads the product catalog once. Does not run in preview.
    pub(crate) fn warm_catalog(&mut self, cx: &mut Context<Self>) {
        if self.catalog_loaded || self.catalog_busy {
            return;
        }
        if cfg!(test) || !self.has_session {
            self.catalog_loaded = true;
            self.catalog_note =
                "Đăng nhập để xem danh mục sản phẩm trên tài khoản Zalo Business.".to_owned();
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            self.catalog_loaded = true;
            self.catalog_note =
                "Đăng nhập để xem danh mục sản phẩm trên tài khoản Zalo Business.".to_owned();
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

    /// One `/api/group/link/detail` while the info column is open. An empty
    /// string is stored on failure so the row stays hidden and is not retried.
    pub(crate) fn drain_info_join(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) || !self.info_open || !self.has_session {
            return;
        }
        let Some(conv) = self
            .selected_conv()
            .filter(|c| c.is_group && c.live && !c.thread_id.is_empty())
        else {
            return;
        };
        let id = conv.id;
        if self.info_join.contains_key(&id) || self.info_join_busy.contains(&id) {
            return;
        }
        if !crate::data::live_allowed(&conv.thread_id) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let grid = conv.thread_id.clone();
        self.info_join_busy.insert(id);
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.get_group_link_detail(&grid)
                            .await
                            .map(|v| crate::account::group_link_from(&v))
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.info_join_busy.remove(&id);
                let url = match res {
                    Ok(link) if link.enabled => link.url,
                    _ => String::new(),
                };
                view.info_join.insert(id, url);
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_join_link(&mut self, url: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(url.to_owned()));
        self.set_note("Đã sao chép link nhóm");
    }

    fn share_join_link(&mut self, url: &str, cx: &mut Context<Self>) {
        let url = url.to_owned();
        self.composer.update(cx, |input, cx| {
            let cur = input.text();
            let next = if cur.trim().is_empty() {
                url.clone()
            } else {
                format!("{cur}\n{url}")
            };
            input.set_text(next, cx);
        });
        self.set_note("Đã chèn link vào ô soạn — chưa gửi");
    }

    fn join_link_row(&self, url: String, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = short_join_url(&url);
        let copy_url = url.clone();
        let share_url = url;
        let row_url = copy_url.clone();
        let ink = token_rgb("--text-primary", "#081b3a");
        let blue = token_rgb("--text-information", "#005ae0");
        let title = crate::color::str_vi("STR_MENU_LINK_JOIN_GROUP", "Link tham gia nhóm")
            .trim()
            .to_owned();
        div()
            .id("info-join-link")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .flex_none()
            .px(px(16.))
            .py(px(8.))
            .min_h(px(56.))
            .cursor_pointer()
            .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.copy_join_link(&row_url, cx);
            }))
            .child(fonts::icon_el(g::LINK, "🔗", 20.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .text_color(rgb(ink))
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(blue))
                            .truncate()
                            .child(shown),
                    ),
            )
            .child(self.info_icon_btn("info-join-copy", g::COPY, cx, move |this, cx| {
                this.copy_join_link(&copy_url, cx);
            }))
            .child(self.info_icon_btn(
                "info-join-share",
                g::SHARE,
                cx,
                move |this, cx| {
                    this.share_join_link(&share_url, cx);
                },
            ))
    }

    fn info_icon_btn(
        &self,
        id: &'static str,
        glyph: char,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex_none()
            .size(px(32.))
            .rounded(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                click(this, cx);
            }))
            .child(fonts::icon_el(glyph, "•", 18.))
    }

    fn info_stack_row(
        &self,
        key: &'static str,
        code: char,
        title: &'static str,
        sub: &str,
        id: u64,
        act: InfoAct,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        div()
            .id(key)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .flex_none()
            .px(px(16.))
            .py(px(8.))
            .min_h(px(56.))
            .cursor_pointer()
            .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.info_action(id, act, cx);
                cx.notify();
            }))
            .child(fonts::icon_el(code, "•", 20.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .text_color(rgb(ink))
                            .truncate()
                            .child(title.trim().to_owned()),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .truncate()
                            .child(sub.trim().to_owned()),
                    ),
            )
    }

    fn view_all_btn(&self, conv: u64, kind: u8, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(("info-view-all", u64::from(kind)))
            .mx(px(16.))
            .mb(px(12.))
            .h(px(32.))
            .flex_none()
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
            .cursor_pointer()
            .hover(|st| st.bg(rgb(token_rgb("--button-secondary-neutral-hover", "#d6d9de"))))
            .text_size(px(14.))
            .font_family(FAMILY_SEMIBOLD)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.info_shelf = Some((conv, kind));
                cx.notify();
            }))
            .child(
                crate::color::str_vi("STR_VIEW_ALL", "Xem tất cả")
                    .trim()
                    .to_owned(),
            )
    }

    /// Ảnh/Video, File and Link, including "Xem tất cả" when the section has rows.
    fn shared_media_block(&self, conv: &ChatConv, cx: &mut Context<Self>) -> impl IntoElement {
        let id = conv.id;
        let thread = conv.thread_id.clone();
        let muted = token_rgb("--text-secondary", "#5a6981");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let cloud_photos = self.cloud_photos(&thread);
        let cloud_files = self.cloud_files(&thread);
        let cloud_links = self.cloud_links(&thread);
        let need_photos = cloud_photos.is_empty();
        let need_files = cloud_files.is_empty();
        let need_links = cloud_links.is_empty();
        let mut local_photos: Vec<crate::data::ChatMsg> = Vec::new();
        let mut local_files: Vec<crate::data::ChatMsg> = Vec::new();
        let mut local_links: Vec<(String, String, i64)> = Vec::new();
        if need_photos || need_files || need_links {
            // Single reverse pass capped to recent messages, stopping as soon as all
            // visible preview slots (8 photos, 3 files, 3 links) are filled.
            for m in self.messages_of(id).iter().rev().take(600) {
                if need_photos
                    && local_photos.len() < 8
                    && matches!(
                        m.kind,
                        crate::media::MsgKind::Photo
                            | crate::media::MsgKind::Doodle
                            | crate::media::MsgKind::Gif
                            | crate::media::MsgKind::Video
                    )
                {
                    local_photos.push(m.clone());
                }
                if need_files
                    && local_files.len() < 3
                    && m.kind == crate::media::MsgKind::File
                {
                    local_files.push(m.clone());
                }
                if need_links && local_links.len() < 3 {
                    let url = if !m.href.is_empty() {
                        Some(m.href.clone())
                    } else if m.text.contains("http") || m.text.contains("www.") || m.text.contains(".com") || m.text.contains(".vn") {
                        crate::media::first_link(&m.text)
                    } else {
                        None
                    };
                    if let Some(url) = url {
                        let title = link_row_title(&m.text, &m.file_name, &url);
                        local_links.push((url, title, m.at_ms));
                    }
                }
                if (!need_photos || local_photos.len() >= 8)
                    && (!need_files || local_files.len() >= 3)
                    && (!need_links || local_links.len() >= 3)
                {
                    break;
                }
            }
        }
        let photo_count = if need_photos {
            local_photos.len()
        } else {
            cloud_photos.len()
        };
        let file_count = if need_files {
            local_files.len()
        } else {
            cloud_files.len()
        };
        let link_count = if need_links {
            local_links.len()
        } else {
            cloud_links.len()
        };
        let cell = ((INFO_PANEL_W - 32.0 - 4.0 * 3.0) / 4.0).floor();
        let mut grid = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_start()
            .gap(px(4.))
            .px(px(16.))
            .pb(px(8.))
            .w_full();
        if photo_count == 0 {
            grid = grid.child(info_empty(
                "Chưa có Ảnh/Video được chia sẻ trong hội thoại này",
                muted,
            ));
        } else if cloud_photos.is_empty() {
            grid = grid.children(local_photos.into_iter().take(8).map(|m| {
                let open = m.clone();
                let thumb = self.media_bitmaps.get(&m.id).cloned();
                square_photo(
                    ("info-photo", m.id),
                    cell,
                    thumb,
                    subtle,
                    muted,
                    cx,
                    move |this, _, window, cx| {
                        this.open_viewer(open.clone(), window, cx);
                        cx.notify();
                    },
                )
            }));
        } else {
            let thread_open = thread.clone();
            grid = grid.children(cloud_photos.into_iter().enumerate().map(|(i, item)| {
                let thumb = self.store_thumb(&item.id);
                let open = item.clone();
                let thread_open = thread_open.clone();
                square_photo(
                    ("info-cloud-photo", i as u64),
                    cell,
                    thumb,
                    subtle,
                    muted,
                    cx,
                    move |this, _, window, cx| {
                        this.open_store_item(&open, id, &thread_open, window, cx);
                        cx.notify();
                    },
                )
            }));
        }
        let mut files = div().flex().flex_col().w_full();
        if file_count == 0 {
            files = files.child(info_empty(
                "Chưa có File được chia sẻ trong hội thoại này",
                muted,
            ));
        } else if cloud_files.is_empty() {
            files = files.children(local_files.into_iter().take(3).enumerate().map(|(i, m)| {
                let name = file_row_name(&m.file_name, &m.text);
                let open = m.media_url.clone();
                dated_row(
                    ("info-file", i as u64),
                    file_mark(&name),
                    name,
                    if m.file_size > 0 {
                        crate::media::format_file_size(m.file_size)
                    } else {
                        String::new()
                    },
                    true,
                    shelf_ymd(m.at_ms),
                    cx,
                    move |_, _, _, cx| {
                        if open.starts_with("http") {
                            cx.open_url(&open);
                        }
                    },
                )
            }));
        } else {
            let thread_open = thread.clone();
            files = files.children(cloud_files.into_iter().enumerate().map(|(i, item)| {
                let name = if item.title.is_empty() {
                    crate::color::str_vi("STR_FILE", "File").to_owned()
                } else {
                    item.title.clone()
                };
                let open = item.clone();
                let thread_open = thread_open.clone();
                dated_row(
                    ("info-cloud-file", i as u64),
                    file_mark(&name),
                    name,
                    if item.file_size > 0 {
                        crate::media::format_file_size(item.file_size)
                    } else {
                        String::new()
                    },
                    true,
                    shelf_ymd(item.sent_ms),
                    cx,
                    move |this, _, window, cx| {
                        this.open_store_item(&open, id, &thread_open, window, cx);
                        cx.notify();
                    },
                )
            }));
        }
        let mut links = div().flex().flex_col().w_full();
        if link_count == 0 {
            links = links.child(info_empty(
                "Chưa có Link được chia sẻ trong hội thoại này",
                muted,
            ));
        } else if cloud_links.is_empty() {
            links = links.children(local_links.into_iter().take(3).enumerate().map(
                |(i, (url, title, at))| {
                    let open = url.clone();
                    let host = link_host_label(&url);
                    dated_row(
                        ("info-link", i as u64),
                        link_mark(),
                        title,
                        host,
                        false,
                        shelf_ymd(at),
                        cx,
                        move |_, _, _, cx| cx.open_url(&open),
                    )
                },
            ));
        } else {
            let thread_open = thread.clone();
            links = links.children(cloud_links.into_iter().enumerate().map(|(i, item)| {
                let host = link_host_label(&item.url);
                let title = if item.title.trim().is_empty() || item.title == item.url {
                    short_join_url(&item.url)
                } else {
                    item.title.clone()
                };
                let open = item.clone();
                let thread_open = thread_open.clone();
                dated_row(
                    ("info-cloud-link", i as u64),
                    link_mark(),
                    title,
                    host,
                    false,
                    shelf_ymd(item.sent_ms),
                    cx,
                    move |this, _, window, cx| {
                        this.open_store_item(&open, id, &thread_open, window, cx);
                        cx.notify();
                    },
                )
            }));
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .child(info_section_head(crate::color::str_vi(
                "STR_MENU_PHOTO_VIDEO",
                "Ảnh/Video",
            )))
            .child(grid)
            .when(photo_count > 0, |el| el.child(self.view_all_btn(id, 0, cx)))
            .child(div().h(px(8.)).flex_none().bg(rgb(subtle)))
            .child(info_section_head(crate::color::str_vi("STR_FILE", "File")))
            .child(files)
            .when(file_count > 0, |el| el.child(self.view_all_btn(id, 1, cx)))
            .child(div().h(px(8.)).flex_none().bg(rgb(subtle)))
            .child(info_section_head(crate::color::str_vi(
                "STR_LINK",
                "Link",
            )))
            .child(links)
            .when(link_count > 0, |el| el.child(self.view_all_btn(id, 2, cx)))
            .child(div().h(px(8.)).flex_none().bg(rgb(subtle)))
    }

    /// Full photo, file or link list already fetched for this conversation.
    fn media_shelf_panel(
        &self,
        conv: &ChatConv,
        kind: u8,
        corners: CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let id = conv.id;
        let thread = conv.thread_id.clone();
        let title = match kind {
            1 => crate::color::str_vi("STR_FILE", "File"),
            2 => crate::color::str_vi("STR_LINK", "Link"),
            _ => crate::color::str_vi("STR_MENU_PHOTO_VIDEO", "Ảnh/Video"),
        };
        let body = if kind == 1 {
            self.shelf_file_list(id, &thread, muted, cx)
        } else if kind == 2 {
            self.shelf_link_list(id, &thread, muted, cx)
        } else {
            self.shelf_photo_grid(id, &thread, subtle, muted, cx)
        };
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
                            .id("info-shelf-back")
                            .size(px(32.))
                            .rounded(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.info_shelf = None;
                                cx.notify();
                            }))
                            .child(fonts::icon_el('\u{EA8E}', "‹", 20.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_center()
                            .mr(px(32.))
                            .text_size(px(18.))
                            .font_family(FAMILY_SEMIBOLD)
                            .child(title.trim().to_owned()),
                    ),
            )
            .child(
                div()
                    .id("info-shelf-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .pt(px(12.))
                    .pb(px(16.))
                    .child(body),
            )
    }

    fn shelf_photo_grid(
        &self,
        id: u64,
        thread: &str,
        subtle: u32,
        muted: u32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let cloud = self.cloud_images_all(thread);
        let cell = ((INFO_PANEL_W - 32.0 - 4.0 * 3.0) / 4.0).floor();
        let mut grid = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_start()
            .gap(px(4.))
            .px(px(16.))
            .w_full();
        if cloud.is_empty() {
            let local: Vec<_> = self
                .messages_of(id)
                .iter()
                .rev()
                .filter(|m| {
                    matches!(
                        m.kind,
                        crate::media::MsgKind::Photo
                            | crate::media::MsgKind::Doodle
                            | crate::media::MsgKind::Gif
                            | crate::media::MsgKind::Video
                    )
                })
                .cloned()
                .collect();
            if local.is_empty() {
                grid = grid.child(info_empty(
                    "Chưa có Ảnh/Video được chia sẻ trong hội thoại này",
                    muted,
                ));
            } else {
                grid = grid.children(local.into_iter().enumerate().map(|(i, m)| {
                    let open = m.clone();
                    let thumb = self.media_bitmaps.get(&m.id).cloned();
                    square_photo(
                        ("shelf-photo", i as u64),
                        cell,
                        thumb,
                        subtle,
                        muted,
                        cx,
                        move |this, _, window, cx| {
                            this.open_viewer(open.clone(), window, cx);
                            cx.notify();
                        },
                    )
                }));
            }
        } else {
            let thread_open = thread.to_owned();
            grid = grid.children(cloud.into_iter().enumerate().map(|(i, item)| {
                let thumb = self.store_thumb(&item.id);
                let open = item.clone();
                let del_id = item.id.clone();
                let can_del = !del_id.is_empty();
                let thread_open = thread_open.clone();
                let thread_del = thread_open.clone();
                div()
                    .relative()
                    .size(px(cell))
                    .child(square_photo(
                        ("shelf-cloud-photo", i as u64),
                        cell,
                        thumb,
                        subtle,
                        muted,
                        cx,
                        move |this, _, window, cx| {
                            this.open_store_item(&open, id, &thread_open, window, cx);
                            cx.notify();
                        },
                    ))
                    .when(can_del, |tile| {
                        tile.child(
                            div()
                                .id(("shelf-cloud-photo-del", i as u64))
                                .absolute()
                                .top(px(3.))
                                .right(px(3.))
                                .size(px(18.))
                                .rounded(px(9.))
                                .bg(gpui::hsla(0., 0., 0., 0.62))
                                .hover(|s| s.bg(rgb(token_rgb("--R60", "#c31818"))))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_size(px(11.))
                                .text_color(rgb(0xff_ff_ff))
                                .tooltip(|_, cx| crate::widgets::tip("Xóa khỏi kho lưu trữ", cx))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.remove_media_store_item(
                                        &thread_del,
                                        "media",
                                        del_id.clone(),
                                        cx,
                                    );
                                    cx.notify();
                                }))
                                .child("×"),
                        )
                    })
            }));
        }
        let can_more = self.media_store_can_load_more(thread, "media");
        let loading_more = self.media_shelf.loading_more;
        let thread_more = thread.to_owned();
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(grid)
            .when(can_more, |col| {
                col.child(self.shelf_load_more_btn(
                    "shelf-more-media",
                    if loading_more {
                        "Đang tải…"
                    } else {
                        "Tải thêm ảnh/video"
                    },
                    !loading_more,
                    thread_more,
                    "media",
                    cx,
                ))
            })
            .into_any_element()
    }

    fn shelf_file_list(
        &self,
        id: u64,
        thread: &str,
        muted: u32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let cloud = self.cloud_files_all(thread);
        let mut col = div().flex().flex_col().w_full();
        if cloud.is_empty() {
            let local: Vec<_> = self
                .messages_of(id)
                .iter()
                .rev()
                .filter(|m| m.kind == crate::media::MsgKind::File)
                .cloned()
                .collect();
            if local.is_empty() {
                col = col.child(info_empty(
                    "Chưa có File được chia sẻ trong hội thoại này",
                    muted,
                ));
            } else {
                col = col.children(local.into_iter().enumerate().map(|(i, m)| {
                    let name = file_row_name(&m.file_name, &m.text);
                    let open = m.media_url.clone();
                    dated_row(
                        ("shelf-file", i as u64),
                        file_mark(&name),
                        name,
                        if m.file_size > 0 {
                            crate::media::format_file_size(m.file_size)
                        } else {
                            String::new()
                        },
                        true,
                        shelf_ymd(m.at_ms),
                        cx,
                        move |_, _, _, cx| {
                            if open.starts_with("http") {
                                cx.open_url(&open);
                            }
                        },
                    )
                }));
            }
        } else {
            let thread_open = thread.to_owned();
            let hover = token_rgb("--layer-background-hover", "#f1f2f4");
            let danger = token_rgb("--R60", "#c31818");
            col = col.children(cloud.into_iter().enumerate().map(|(i, item)| {
                let name = if item.title.is_empty() {
                    crate::color::str_vi("STR_FILE", "File").to_owned()
                } else {
                    item.title.clone()
                };
                let open = item.clone();
                let del_id = item.id.clone();
                let can_del = !del_id.is_empty();
                let thread_open = thread_open.clone();
                let thread_del = thread_open.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .child(
                        div().flex_1().min_w(px(0.)).child(dated_row(
                            ("shelf-cloud-file", i as u64),
                            file_mark(&name),
                            name,
                            if item.file_size > 0 {
                                crate::media::format_file_size(item.file_size)
                            } else {
                                String::new()
                            },
                            true,
                            shelf_ymd(item.sent_ms),
                            cx,
                            move |this, _, window, cx| {
                                this.open_store_item(&open, id, &thread_open, window, cx);
                                cx.notify();
                            },
                        )),
                    )
                    .when(can_del, |row| {
                        row.child(
                            div()
                                .id(("shelf-cloud-file-del", i as u64))
                                .flex_none()
                                .mr(px(10.))
                                .size(px(26.))
                                .rounded(px(13.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_color(rgb(muted))
                                .hover(move |s| s.bg(rgb(hover)).text_color(rgb(danger)))
                                .tooltip(|_, cx| crate::widgets::tip("Xóa khỏi kho lưu trữ", cx))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.remove_media_store_item(
                                        &thread_del,
                                        "file",
                                        del_id.clone(),
                                        cx,
                                    );
                                    cx.notify();
                                }))
                                .child(fonts::icon_el(g::TRASH, "×", 14.)),
                        )
                    })
            }));
        }
        if self.media_store_can_load_more(thread, "file") {
            let loading_more = self.media_shelf.loading_more;
            col = col.child(self.shelf_load_more_btn(
                "shelf-more-file",
                if loading_more {
                    "Đang tải…"
                } else {
                    "Tải thêm file"
                },
                !loading_more,
                thread.to_owned(),
                "file",
                cx,
            ));
        }
        col.into_any_element()
    }

    fn shelf_link_list(
        &self,
        id: u64,
        thread: &str,
        muted: u32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let cloud = self.cloud_links_all(thread);
        let mut col = div().flex().flex_col().w_full();
        if cloud.is_empty() {
            let local: Vec<(String, String, i64)> = self
                .messages_of(id)
                .iter()
                .rev()
                .filter_map(|m| {
                    let url = if !m.href.is_empty() {
                        m.href.clone()
                    } else {
                        crate::media::first_link(&m.text)?
                    };
                    Some((
                        url.clone(),
                        link_row_title(&m.text, &m.file_name, &url),
                        m.at_ms,
                    ))
                })
                .collect();
            if local.is_empty() {
                col = col.child(info_empty(
                    "Chưa có Link được chia sẻ trong hội thoại này",
                    muted,
                ));
            } else {
                col = col.children(local.into_iter().enumerate().map(|(i, (url, title, at))| {
                    let open = url.clone();
                    dated_row(
                        ("shelf-link", i as u64),
                        link_mark(),
                        title,
                        link_host_label(&url),
                        false,
                        shelf_ymd(at),
                        cx,
                        move |_, _, _, cx| cx.open_url(&open),
                    )
                }));
            }
        } else {
            let thread_open = thread.to_owned();
            let hover = token_rgb("--layer-background-hover", "#f1f2f4");
            let danger = token_rgb("--R60", "#c31818");
            col = col.children(cloud.into_iter().enumerate().map(|(i, item)| {
                let host = link_host_label(&item.url);
                let title = if item.title.trim().is_empty() || item.title == item.url {
                    short_join_url(&item.url)
                } else {
                    item.title.clone()
                };
                let open = item.clone();
                let del_id = item.id.clone();
                let can_del = !del_id.is_empty();
                let thread_open = thread_open.clone();
                let thread_del = thread_open.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .child(
                        div().flex_1().min_w(px(0.)).child(dated_row(
                            ("shelf-cloud-link", i as u64),
                            link_mark(),
                            title,
                            host,
                            false,
                            shelf_ymd(item.sent_ms),
                            cx,
                            move |this, _, window, cx| {
                                this.open_store_item(&open, id, &thread_open, window, cx);
                                cx.notify();
                            },
                        )),
                    )
                    .when(can_del, |row| {
                        row.child(
                            div()
                                .id(("shelf-cloud-link-del", i as u64))
                                .flex_none()
                                .mr(px(10.))
                                .size(px(26.))
                                .rounded(px(13.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_color(rgb(muted))
                                .hover(move |s| s.bg(rgb(hover)).text_color(rgb(danger)))
                                .tooltip(|_, cx| crate::widgets::tip("Xóa khỏi kho lưu trữ", cx))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.remove_media_store_item(
                                        &thread_del,
                                        "link",
                                        del_id.clone(),
                                        cx,
                                    );
                                    cx.notify();
                                }))
                                .child(fonts::icon_el(g::TRASH, "×", 14.)),
                        )
                    })
            }));
        }
        if self.media_store_can_load_more(thread, "link") {
            let loading_more = self.media_shelf.loading_more;
            col = col.child(self.shelf_load_more_btn(
                "shelf-more-link",
                if loading_more {
                    "Đang tải…"
                } else {
                    "Tải thêm link"
                },
                !loading_more,
                thread.to_owned(),
                "link",
                cx,
            ));
        }
        col.into_any_element()
    }

    fn shelf_load_more_btn(
        &self,
        id: &'static str,
        label: &'static str,
        enabled: bool,
        thread: String,
        media_type: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bg = token_rgb("--layer-background-subtle", "#eaedf0");
        let hover = token_rgb("--layer-background-hover", "#dfe2e7");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        div()
            .id(id)
            .mx(px(16.))
            .mt(px(10.))
            .mb(px(8.))
            .h(px(34.))
            .rounded(px(4.))
            .bg(rgb(bg))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.))
            .font_family(FAMILY_SEMIBOLD)
            .text_color(rgb(if enabled { ink } else { muted }))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .hover(move |s| s.bg(rgb(hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.load_more_media_store(&thread, media_type, cx);
                        cx.notify();
                    }))
            })
            .child(label)
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
                                                    token_rgb("--B55", "#0068ff"),
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

    #[test]
    fn join_and_link_labels_drop_scheme() {
        assert_eq!(
            short_join_url("https://zalo.me/g/qpbstn093"),
            "zalo.me/g/qpbstn093"
        );
        assert_eq!(short_join_url("http://zalo.me/g/abc/"), "zalo.me/g/abc");
        assert_eq!(
            link_host_label("https://www.github.com/duw/tool"),
            "github.com"
        );
        assert_eq!(link_host_label("https://t.co/abc"), "t.co");
        assert_eq!(
            link_host_label("https://tokenized.play.google.com/ea"),
            "tokenized.play.google.com"
        );
        assert_eq!(
            link_row_title("https://github.com/a", "", "https://github.com/a"),
            "github.com/a"
        );
        assert_eq!(
            file_row_name("", "[File] BAO-CAO.md"),
            "BAO-CAO.md"
        );
    }
}
