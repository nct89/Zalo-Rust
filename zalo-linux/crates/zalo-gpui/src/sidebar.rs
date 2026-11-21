//! Conversation list 344 px (`#sidebarNav` 408 − `#main-tab` 64).

use gpui::{
    div, hsla, prelude::*, px, rgb, ClickEvent, Context, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};

use crate::color::{str_vi, token_rgb};
use crate::context_menu::MenuKind;
use crate::data::ChatConv;
use crate::fonts::{self, ic, FAMILY_ICON, FAMILY_SEMIBOLD};
use crate::rail::{lists_chats, Rail};
use crate::shell::ShellView;
use crate::widgets::tip;
use crate::window_frame::CsdCorners;

/// Four Contacts entries, same order as Zalo PC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContactTab {
    /// Friends list.
    #[default]
    Friends,
    /// Groups & Communities list.
    Groups,
    /// Friend requests.
    Requests,
    /// Group & Community invitations.
    Invites,
}

impl ContactTab {
    fn label(self) -> &'static str {
        match self {
            Self::Friends => str_vi("STR_FRIEND_LIST", "Danh sách bạn bè"),
            Self::Groups => str_vi(
                "STR_CONTACT_TAB_GROUP_COMMUNITY_LIST",
                "Danh sách nhóm và cộng đồng",
            ),
            Self::Requests => str_vi("STR_FRIEND_REQUEST_DEFAULT", "Lời mời kết bạn"),
            Self::Invites => str_vi(
                "STR_CONTACT_TAB_GROUP_COMMUNITY_INVITATIONS",
                "Lời mời vào nhóm và cộng đồng",
            ),
        }
    }

    /// Title shown in the right-hand contacts page.
    pub fn page_title(self) -> &'static str {
        self.label()
    }

    fn glyph(self) -> (char, &'static str) {
        match self {
            Self::Friends => (ic::FRIEND_LIST, "☺"),
            Self::Groups => (ic::GROUP_LIST, "☰"),
            Self::Requests => (ic::FRIEND_REQ, "+"),
            Self::Invites => (ic::GROUP_REQ, "☰"),
        }
    }
}

/// Sidebar preview for location / card messages: the place or card title.
fn preview_marked(text: &str, title: &str, fallback: &str) -> String {
    if !title.is_empty() {
        return title.to_owned();
    }
    let rest = text
        .trim()
        .split_once(']')
        .map(|(_, rest)| rest.trim())
        .unwrap_or("");
    if rest.is_empty() {
        fallback.to_owned()
    } else {
        rest.to_owned()
    }
}

impl ShellView {
    /// `#contact-search` + tabs + rows, or the four contact entries.
    pub(crate) fn sidebar_column(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let line = crate::color::border_subtle();
        div()
            .id("conv-list")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(zalo_design::CONV_LIST_INNER_W))
            .h_full()
            .bg(rgb(paper))
            .child(div().w(px(1.)).h_full().absolute().right_0().bg(line))
            .child(self.search_bar(cx))
            .when(lists_chats(self.rail), |el| {
                el.child(self.list_tabs(cx))
                    .when_some(self.sync_banner_el(cx), |el, banner| el.child(banner))
                    .when_some(self.backup_banner_el(cx), |el, banner| el.child(banner))
                    .child(self.conv_rows(cx))
            })
            .when(self.rail == Rail::Contacts, |el| {
                el.child(self.contacts_nav(cx))
            })
    }

    fn search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // `#contact-search` 64 tall, padding 0 16; input 32 tall, radius 5,
        // `#f1f2f4`, icon 14 px at +12, placeholder `#77859c`; two 32×32
        // buttons (radius 3, icon 20 px, ink) 4 px apart (live DOM).
        let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
        let placeholder = token_rgb("--text-placeholder", "#77859c");
        let add = str_vi("STR_TITLE_ADD_FRIEND", "Thêm bạn");
        let group = str_vi("STR_CREATE_GROUP", "Tạo nhóm");
        div()
            .id("contact-search")
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(zalo_design::CONTACT_SEARCH_H))
            .px(px(zalo_design::CONTACT_SEARCH_PAD_X))
            .gap(px(4.))
            .child(
                div()
                    .id("contact-search-input")
                    .flex()
                    .flex_1()
                    .flex_row()
                    .items_center()
                    .mr(px(4.))
                    .h(px(zalo_design::GROUP_SEARCH_H))
                    .rounded(px(zalo_design::SEARCH_INPUT_RADIUS))
                    .bg(rgb(field))
                    .text_size(px(14.))
                    .text_color(rgb(placeholder))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .ml(px(12.))
                            .mr(px(6.))
                            .text_color(rgb(token_rgb("--icon-primary", "#081b3a")))
                            .child(fonts::icon_el(
                                ic::SEARCH_FIELD,
                                "⌕",
                                zalo_design::SEARCH_FIELD_ICON,
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .pr(px(12.))
                            .child(self.search.clone()),
                    ),
            )
            .child(
                tool_btn("search-add", ic::ADD_CONTACT, "+", add, 32., 20., 3.).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.open_add_friend(cx);
                        cx.notify();
                    }),
                ),
            )
            .child(
                tool_btn("search-group", ic::GROUP_ADD, "☰", group, 32., 20., 3.).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.open_group_picker(None, cx);
                        cx.notify();
                    }),
                ),
            )
    }

    fn list_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // `.msg-filters-bar` 32 tall, padding 0 16, bottom border α .15;
        // tabs 14 px / 500, second tab margin-left 12, selected `#005ae0` with
        // a 2 px `#0068ff` indicator; right: "Labels ⌄" + "…" 24×24.
        let other = self.other_tab;
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B60", "#005ae0");
        let filter_label = match self.list_filter {
            crate::shell::ListFilter::All => crate::i18n::t("Phân loại"),
            crate::shell::ListFilter::Unread => str_vi("STR_UNREAD_LABEL", "Chưa đọc").to_owned(),
            crate::shell::ListFilter::Strangers => "Người lạ".to_owned(),
            crate::shell::ListFilter::Label(id) => self
                .labels
                .iter()
                .find(|l| l.id == id)
                .map(|l| l.text.clone())
                .unwrap_or_else(|| "Phân loại".to_owned()),
        };
        let filtered = self.list_filter != crate::shell::ListFilter::All;
        let split = self.prefs.bool("split", true);
        let unread_tab = self.list_filter == crate::shell::ListFilter::Unread;
        let (first, second, first_on) = if split {
            ("Ưu tiên", "Khác", !other)
        } else {
            ("Tất cả", "Chưa đọc", !unread_tab)
        };
        div()
            .id("msg-filters-bar")
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(32.))
            .px(px(16.))
            .border_b_1()
            .border_color(crate::color::border_subtle())
            .child(
                text_tab("tab-priority", first, first_on, muted, blue).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if split {
                            this.other_tab = false;
                        } else {
                            this.list_filter = crate::shell::ListFilter::All;
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                text_tab("tab-other", second, !first_on, muted, blue)
                    .ml(px(12.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if split {
                            this.other_tab = true;
                        } else {
                            this.list_filter = crate::shell::ListFilter::Unread;
                        }
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("filter-preview")
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(24.))
                    .pl(px(8.))
                    .pr(px(4.))
                    .gap(px(4.))
                    .rounded(px(12.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(rgb(if filtered { blue } else { ink }))
                    .when(filtered, |el| {
                        el.bg(rgb(token_rgb("--layer-background-selected", "#e5f1ff")))
                    })
                    .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        this.open_menu(MenuKind::Filter, event.position(), window);
                        cx.notify();
                    }))
                    .child(filter_label)
                    .child(fonts::icon_el(ic::SOLID_DOWN, "⌄", 16.)),
            )
            .child(
                tool_btn(
                    "tab-more",
                    ic::SOLID_MORE,
                    "…",
                    str_vi("STR_MORE_OPTION_TIP", "Tùy chọn thêm"),
                    24.,
                    16.,
                    12.,
                )
                .ml(px(4.))
                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                    this.open_menu(MenuKind::TabMore, event.position(), window);
                    cx.notify();
                })),
            )
    }

    fn conv_rows(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected;
        let indices = std::rc::Rc::new(self.listed_conv_indices());
        let count = indices.len();
        let show_back = self.list_filter == crate::shell::ListFilter::Strangers;
        let indices_for_list = std::rc::Rc::clone(&indices);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .when(show_back, |el| {
                el.child(self.stranger_back_bar(count, cx))
            })
            .child(
                gpui::uniform_list(
                    "conv-scroll",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        range
                            .filter_map(|ix| {
                                let conv_ix = *indices_for_list.get(ix)?;
                                this.convs.get(conv_ix)
                            })
                            .map(|row| {
                                let on = selected == Some(row.id);
                                this.conv_row(row.clone(), on, cx)
                            })
                            .collect()
                    }),
                )
                .track_scroll(self.conv_scroll.clone())
                .flex_1()
                .w_full(),
            )
    }

    fn stranger_back_bar(&self, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let blue = token_rgb("--B55", "#0068ff");
        div()
            .id("stranger-back-bar")
            .w_full()
            .min_h(px(36.))
            .px(px(16.))
            .py(px(6.))
            .bg(rgb(token_rgb("--layer-background-subtle", "#f4f5f7")))
            .border_b_1()
            .border_color(crate::color::border_vanish())
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(13.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child(format!("Tin nhắn từ người lạ ({count})")),
            )
            .child(
                div()
                    .id("stranger-back-btn")
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(4.))
                    .bg(rgb(token_rgb("--layer-background-selected", "#e5efff")))
                    .hover(|s| s.bg(rgb(token_rgb("--layer-background-hover", "#d4e5ff"))))
                    .text_color(rgb(blue))
                    .text_size(px(12.))
                    .font_family(FAMILY_SEMIBOLD)
                    .cursor_pointer()
                    .child("Quay lại ×")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.list_filter = crate::shell::ListFilter::All;
                        cx.notify();
                    })),
            )
    }

    #[allow(dead_code)]
    fn stranger_box_row(
        &self,
        count: usize,
        unread: u32,
        time: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let blue = token_rgb("--B55", "#0068ff");
        let red = token_rgb("--R60", "#c31818");
        div()
            .id("stranger-box-row")
            .w_full()
            .h(px(zalo_design::CONV_ITEM_H))
            .px(px(16.))
            .flex()
            .flex_row()
            .items_center()
            .cursor_pointer()
            .hover(|st| st.bg(rgb(hover)))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.list_filter = crate::shell::ListFilter::Strangers;
                cx.notify();
            }))
            .child(
                div()
                    .size(px(48.))
                    .rounded(px(24.))
                    .flex_none()
                    .bg(rgb(token_rgb("--layer-background-selected", "#e5efff")))
                    .text_color(rgb(blue))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(fonts::icon_el(ic::USER, "👤", 22.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .ml(px(12.))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(zalo_design::CONV_TITLE_PX))
                                    .line_height(px(24.))
                                    .text_color(rgb(ink))
                                    .when(unread > 0, |el| el.font_family(FAMILY_SEMIBOLD))
                                    .truncate()
                                    .child("Tin nhắn từ người lạ"),
                            )
                            .when(!time.is_empty(), |el| {
                                el.child(
                                    div()
                                        .ml(px(6.))
                                        .flex_none()
                                        .text_size(px(zalo_design::CONV_TIME_PX))
                                        .text_color(rgb(if unread > 0 { ink } else { muted }))
                                        .when(unread > 0, |e| e.font_family(FAMILY_SEMIBOLD))
                                        .child(time),
                                )
                            }),
                    )
                    .child(
                        div()
                            .mt(px(2.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(14.))
                                    .line_height(px(21.))
                                    .text_color(rgb(if unread > 0 { ink } else { muted }))
                                    .truncate()
                                    .child(format!("{count} cuộc trò chuyện")),
                            )
                            .when(unread > 0, |el| el.child(count_badge(unread, red))),
                    ),
            )
    }

    /// Group preview prefix. A stored "Người dùng Zalo" or raw id uses the
    /// profile name when that person is already in the cache.
    fn preview_sender(&self, last: &crate::data::ChatMsg) -> String {
        let fallback = str_vi("STR_ZALO_USER", "Người dùng Zalo");
        let raw = last.sender.trim();
        let unknown = raw.is_empty()
            || raw == fallback
            || (!last.sender_uid.is_empty() && crate::profiles::is_id_label(&last.sender_uid, raw));
        if unknown && !last.sender_uid.is_empty() {
            if let Some(name) = self
                .profiles
                .map
                .get(&last.sender_uid)
                .map(|p| p.name.trim().to_owned())
                .filter(|n| !n.is_empty() && !crate::profiles::is_id_label(&last.sender_uid, n))
            {
                return name;
            }
        }
        if raw.is_empty() {
            fallback.to_owned()
        } else {
            raw.to_owned()
        }
    }

    /// Prefix, icon and text of the last-message line (`.z-conv-message`).
    fn preview_parts(&self, row: &ChatConv) -> (Option<String>, Option<(char, bool)>, String) {
        use crate::media::MsgKind;
        let Some(last) = self.messages_of(row.id).last() else {
            return (None, None, row.preview.clone());
        };
        let prefix = if last.from_me {
            Some(format!("{}:", str_vi("STR_YOU", "Bạn")))
        } else if row.is_group && !last.sender.is_empty() {
            Some(format!("{}:", self.preview_sender(last)))
        } else {
            None
        };
        let (icon, text) = match last.kind {
            MsgKind::Photo => (Some((ic::PICTURE, false)), "Hình ảnh".to_owned()),
            MsgKind::Doodle => (Some((ic::PICTURE, false)), "Vẽ hình".to_owned()),
            MsgKind::Location => (None, preview_marked(&last.text, &last.file_name, "Vị trí")),
            MsgKind::Ecard => (None, preview_marked(&last.text, &last.file_name, "Thiệp")),
            MsgKind::Gif => (Some((ic::PICTURE, false)), "GIF".to_owned()),
            MsgKind::Sticker => (Some((ic::STICKER_PREVIEW, false)), "Sticker".to_owned()),
            MsgKind::File => (Some((ic::FILE, false)), last.file_name.clone()),
            MsgKind::Call => {
                if crate::media::is_group_call_body(&last.text) {
                    (Some((ic::CALL_OUT, false)), "Cuộc gọi nhóm".to_owned())
                } else if last.from_me {
                    (Some((ic::CALL_OUT, false)), "Cuộc gọi thoại đi".to_owned())
                } else {
                    (Some((ic::CALL_IN, false)), "Cuộc gọi thoại đến".to_owned())
                }
            }
            MsgKind::MissedCall => (Some((ic::CALL_MISS, true)), "Cuộc gọi nhỡ".to_owned()),
            MsgKind::Voice => (None, "Tin nhắn thoại".to_owned()),
            MsgKind::Video => (None, "Video".to_owned()),
            MsgKind::Poll => {
                let q = if last.file_name.is_empty() {
                    last.text
                        .trim()
                        .trim_start_matches("[Bình chọn]")
                        .trim()
                        .to_owned()
                } else {
                    last.file_name.clone()
                };
                let text = if q.is_empty() {
                    "[Bình chọn]".to_owned()
                } else {
                    format!("[Bình chọn] {q}")
                };
                (None, text)
            }
            MsgKind::Card => (None, "Danh thiếp".to_owned()),
            MsgKind::Link | MsgKind::Text => (None, last.text.replace('\n', " ")),
        };
        let text = if last.recalled {
            str_vi("STR_UNDO_MESSAGE", "Tin nhắn đã thu hồi").to_owned()
        } else {
            text
        };
        (prefix, icon, text)
    }

    fn conv_row(&self, row: ChatConv, on: bool, cx: &mut Context<Self>) -> impl IntoElement {
        // `.msg-item` 78; `.conv-item` margin 4 8, padding 0 8, radius 6;
        // avatar column 60 (avatar 48); name 16 px lh 24 at +12 (500 when
        // unread); time 12 px; preview 14 px lh 21 (ink when unread, else
        // `#5a6981`); unread badge 16 `#c31818`; pin 16 `#5a6981` (live DOM).
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let selected = token_rgb("--layer-background-selected", "#e5efff");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let red = token_rgb("--R60", "#c31818");
        let id = row.id;
        let unread = self.unread_of(&row);
        let pinned = self.pinned.contains(&id);
        let muted_row = self.muted.contains(&id);
        let own_photo = self.shown_photo(&row.thread_id);
        let cloud_row = crate::shell::is_my_documents(
            &row.name,
            &row.thread_id,
            self.send2me_id.as_deref(),
        );
        let member_faces = if row.is_group && own_photo.is_none() && !cloud_row {
            self.group_collage_faces(&row.thread_id)
        } else {
            Vec::new()
        };
        let initial = avatar_initials(&row.name);
        let fill = if row.is_group {
            0x0068ff
        } else {
            avatar_color(&row.thread_id)
        };
        let at = self.messages_of(id).last().map(|m| m.at_ms).unwrap_or(0);
        let time = if at > 0 {
            crate::data::conv_time_rel(at, crate::data::now_ms())
        } else {
            row.time.clone()
        };
        let typing = self.typing_label_for(&row);
        let (prefix, icon, text) = match &typing {
            Some(label) => (None, None, label.clone()),
            None => self.preview_parts(&row),
        };
        let body_color = if typing.is_some() {
            token_rgb("--B55", "#0068ff")
        } else if unread > 0 {
            ink
        } else {
            muted
        };
        let label_dot = self
            .conv_label(id)
            .and_then(|lid| self.labels.iter().find(|l| l.id == lid))
            .and_then(|l| crate::color::parse_hex(&l.color));
        let group = gpui::SharedString::from(format!("conv-row-{id}"));
        // Row stays in its hover state while its context menu is open.
        let menu_on =
            matches!(self.menu.as_ref().map(|m| &m.kind), Some(MenuKind::Conv(x)) if *x == id);
        let hover_btn = |key: &'static str, code: char, label: &'static str| {
            div()
                .id((key, id))
                .size(px(24.))
                .rounded(px(3.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(ink))
                .hover(|st| st.bg(rgb(token_rgb("--B10", "#e5efff"))))
                .tooltip(move |_, cx| tip(label, cx))
                .child(fonts::icon_el(code, "•", 16.))
        };
        div()
            .id(("conv", id))
            .group(group.clone())
            .flex()
            .flex_none()
            .w_full()
            .h(px(zalo_design::CONV_ITEM_H))
            .px(px(zalo_design::CONV_ITEM_MARGIN_X))
            .py(px(4.))
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    this.open_menu(MenuKind::Conv(id), event.position, window);
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if this.conv_hidden(id) {
                    this.open_pin(crate::privacy::PinMode::Unlock { conv: Some(id) }, cx);
                } else {
                    this.select_conv(id);
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .h_full()
                    .px(px(zalo_design::CONV_ITEM_PADDING_X))
                    .rounded(px(zalo_design::CONV_ITEM_RADIUS))
                    .when(on, |el| el.bg(rgb(selected)))
                    .when(!on, |el| el.hover(|style| style.bg(rgb(hover))))
                    .when(!on && menu_on, |el| el.bg(rgb(hover)))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .w(px(60.))
                            .h_full()
                            .child(avatar(
                                &initial,
                                fill,
                                self.conv_online(&row),
                                row.is_group,
                                own_photo,
                                cloud_row,
                                &member_faces,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .pt(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .h(px(24.))
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .text_size(px(zalo_design::CONV_TITLE_PX))
                                            .line_height(px(24.))
                                            .text_color(rgb(ink))
                                            .when(unread > 0, |el| el.font_family(FAMILY_SEMIBOLD))
                                            .truncate()
                                            .child(if cloud_row {
                                                "My Documents".to_owned()
                                            } else {
                                                row.name.clone()
                                            }),
                                    )
                                    .child(
                                        // `#conv-title-action`: "Open in Separate Window" + "…" replace
                                        // the time while the row is hovered (live DOM 48 × 24).
                                        div()
                                            .relative()
                                            .flex_none()
                                            .min_w(px(48.))
                                            .flex()
                                            .justify_end()
                                            .child(
                                                div()
                                                    .text_size(px(zalo_design::CONV_TIME_PX))
                                                    .text_color(rgb(if unread > 0 { ink } else { muted }))
                                                    .when(unread > 0, |el| el.font_family(FAMILY_SEMIBOLD))
                                                    .group_hover(group.clone(), |st| st.invisible())
                                                    .when(menu_on, |el| el.invisible())
                                                    .child(time),
                                            )
                                            .child(
                                                div()
                                                    .absolute()
                                                    .top(px(-2.))
                                                    .right_0()
                                                    .flex()
                                                    .flex_row()
                                                    .when(!menu_on, |el| el.invisible())
                                                    .group_hover(group.clone(), |st| st.visible())
                                                    .child(hover_btn("conv-solo", ic::CONV_EXTEND, "Mở cửa sổ riêng").on_click(cx.listener(
                                                        move |this, _: &ClickEvent, _, cx| {
                                                            this.open_solo(id, cx);
                                                            cx.stop_propagation();
                                                            cx.notify();
                                                        },
                                                    )))
                                                    .child(hover_btn("conv-more", ic::CONV_MORE, "Thêm").on_click(cx.listener(
                                                        move |this, e: &ClickEvent, window, cx| {
                                                            this.open_menu(MenuKind::Conv(id), e.position(), window);
                                                            cx.stop_propagation();
                                                            cx.notify();
                                                        },
                                                    ))),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .mt(px(4.))
                                    .h(px(21.))
                                    .gap(px(4.))
                                    .text_size(px(zalo_design::CONV_PREVIEW_PX))
                                    .line_height(px(21.))
                                    .text_color(rgb(body_color))
                                    .when_some(label_dot, |el, color| {
                                        el.child(div().flex_none().size(px(10.)).rounded(px(2.)).bg(rgb(color)))
                                    })
                                    .when(muted_row, |el| {
                                        el.child(fonts::icon_el(ic::SOLID_MUTED, "🔕", 14.))
                                    })
                                    .when_some(prefix, |el, p| {
                                        el.child(div().flex_none().text_color(rgb(body_color)).child(p))
                                    })
                                    .when_some(icon, |el, (code, red_icon)| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .text_color(rgb(if red_icon { red } else { body_color }))
                                                .child(fonts::icon_el(code, "•", 16.)),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .truncate()
                                            .pr(px(4.))
                                            .child(crate::i18n::t(&text)),
                                    )
                                    .when(unread > 0, |el| el.child(count_badge(unread, red)))
                                    .when(unread == 0 && pinned, |el| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .text_color(rgb(muted))
                                                .child(fonts::icon_el(ic::PIN, "📌", 16.)),
                                        )
                                    }),
                            ),
                    ),
            )
    }

    fn contacts_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.contact_tab;
        let tabs = [
            ContactTab::Friends,
            ContactTab::Groups,
            ContactTab::Requests,
            ContactTab::Invites,
        ];
        div()
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .pt(px(4.))
            .children(
                tabs.into_iter()
                    .map(|tab| self.contact_entry(tab, tab == on, cx)),
            )
    }

    fn contact_entry(&self, tab: ContactTab, on: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let selected = token_rgb("--layer-background-selected", "#e5f1ff");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let label = tab.label();
        div()
            .id(("contact-tab", tab as u64))
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(zalo_design::CONTACT_ITEM_H))
            .px(px(zalo_design::CONV_ITEM_MARGIN_X))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.contact_tab = tab;
                this.selected = None;
                if tab == ContactTab::Requests {
                    this.load_requests(cx);
                } else if tab == ContactTab::Invites {
                    this.load_invites(cx);
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .h_full()
                    .px(px(16.))
                    .gap(px(12.))
                    .rounded(px(zalo_design::CONV_ITEM_RADIUS))
                    .overflow_hidden()
                    .when(on, |el| el.bg(rgb(selected)))
                    .when(!on, |el| el.hover(|style| style.bg(rgb(hover))))
                    .text_size(px(15.))
                    .text_color(rgb(ink))
                    .child({
                        let (code, fallback) = tab.glyph();
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .text_size(px(zalo_design::SEARCH_ACTION_ICON))
                            .when(fonts::icon_loaded(), |el| el.font_family(FAMILY_ICON))
                            .child(fonts::glyph(code, fallback))
                    })
                    .child(crate::i18n::t(&label)),
            )
    }

    /// Right-hand Contacts page: header + preview list or empty copy.
    pub(crate) fn contacts_page(
        &self,
        corners: CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let ink = token_rgb("--text-primary", "#081b3a");
        let line = crate::color::border_subtle();
        let tab = self.contact_tab;
        let (code, fallback) = tab.glyph();
        corners
            .apply(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .min_w(px(0.))
                    .bg(rgb(subtle)),
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
                    .px(px(16.))
                    .gap(px(12.))
                    .bg(rgb(paper))
                    .border_b_1()
                    .border_color(line)
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .text_size(px(zalo_design::SEARCH_ACTION_ICON))
                            .text_color(rgb(ink))
                            .when(fonts::icon_loaded(), |el| el.font_family(FAMILY_ICON))
                            .child(fonts::glyph(code, fallback)),
                    )
                    .child(
                        div()
                            .text_size(px(17.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child(tab.page_title()),
                    ),
            )
            .child(match tab {
                ContactTab::Friends | ContactTab::Groups => self
                    .contacts_list(tab == ContactTab::Groups, cx)
                    .into_any_element(),
                ContactTab::Requests => self.requests_page(cx).into_any_element(),
                ContactTab::Invites => self.invites_page(cx).into_any_element(),
            })
    }
}

/// `.z--btn--v2.btn-tertiary-neutral`: square button, ink icon.
fn tool_btn(
    id: &'static str,
    code: char,
    fallback: &'static str,
    label: &'static str,
    size: f32,
    icon: f32,
    radius: f32,
) -> gpui::Stateful<gpui::Div> {
    let hover = token_rgb("--layer-background-hover", "#f1f2f4");
    let ink = token_rgb("--text-primary", "#081b3a");
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(radius))
        .text_color(rgb(ink))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(hover)))
        .tooltip(move |_, cx| tip(label, cx))
        .child(fonts::icon_el(code, fallback, icon))
}

fn text_tab(
    id: &'static str,
    label: &'static str,
    on: bool,
    idle: u32,
    active: u32,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .h_full()
        .mb(px(-1.))
        .text_size(px(14.))
        .line_height(px(21.))
        .font_family(FAMILY_SEMIBOLD)
        .cursor_pointer()
        .border_b_2()
        .when(on, |el| {
            el.text_color(rgb(active))
                .border_color(rgb(token_rgb("--B55", "#0068ff")))
        })
        .when(!on, |el| {
            el.text_color(rgb(idle)).border_color(hsla(0., 0., 0., 0.))
        })
        .child(crate::i18n::t(&label))
}

/// Red counter (`.z-noti-badge --big --counter`), 16 px tall.
fn count_badge(count: u32, red: u32) -> impl IntoElement {
    let text = if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    };
    div()
        .flex_none()
        .h(px(16.))
        .min_w(px(16.))
        .px(px(if text.len() > 1 { 4. } else { 0. }))
        .rounded(px(8.))
        .bg(rgb(red))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .line_height(px(16.))
        .font_family(FAMILY_SEMIBOLD)
        .text_color(rgb(0xffffff))
        .child(crate::i18n::t(&text))
}

/// Blue circle plus white `MyDocs_24_Filled` for "My Cloud".
/// `MyDocument_Ava` is a solid disc, so it cannot sit on this circle.
fn cloud_avatar(size: f32) -> impl IntoElement {
    div()
        .size(px(size))
        .rounded(px(size / 2.0))
        .bg(rgb(0x0068ff))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(0xffffff))
        .child(fonts::icon_el(ic::DOCS_ON, "☁", size * 0.68))
}

/// What to paint when a thread has no single photo of its own.
///
/// Electron's avatar widget (`zavatar-multi`) collages `topMember` only when
/// the group has no `avatar`. A group with neither photo nor member faces
/// uses the group glyph. A person still gets an initial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AvatarPaint {
    Photo,
    Cloud,
    Collage,
    GroupMark,
    Letter,
}

#[must_use]
pub(crate) fn avatar_paint(is_group: bool, has_photo: bool, cloud: bool, faces: usize) -> AvatarPaint {
    if has_photo {
        AvatarPaint::Photo
    } else if cloud {
        AvatarPaint::Cloud
    } else if is_group && faces > 0 {
        AvatarPaint::Collage
    } else if is_group {
        AvatarPaint::GroupMark
    } else {
        AvatarPaint::Letter
    }
}

/// One cell inside a group collage. Electron draws these as overlapping
/// circles (`zavatar-multi`), not as slices of one circle.
#[derive(Clone)]
pub(crate) enum CollageFace {
    Photo(std::sync::Arc<gpui::RenderImage>),
    /// Coloured disc. `text` is empty when the member has no display name.
    Letter { text: String, fill: u32 },
    /// The `99+` cell when `totalMember` is above 4, or the extra cell that
    /// keeps a one-member group on the two-circle layout.
    Count(u32),
    /// Padded member Electron paints with the default user glyph.
    Placeholder,
}

/// Which member indexes a collage shows. Mirrors `YfZ7` `m()` after
/// `_sortMemberIds`: creator, then admins, then the rest. At most four cells.
/// A counter replaces the fourth face when the group is larger than 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollagePick {
    /// Index into the original id slice, not into the sorted order.
    Face(usize),
    Count(u32),
    Placeholder,
}

/// `_sortMemberIds`: creator, then `adminIds` in server order, then everyone
/// else. A person already placed is not repeated.
fn sorted_member_indexes(
    ids: &[impl AsRef<str>],
    creator: &str,
    admins: &[impl AsRef<str>],
) -> Vec<usize> {
    let mut taken = vec![false; ids.len()];
    let mut out = Vec::with_capacity(ids.len());
    let mut push_id = |id: &str| {
        if id.is_empty() {
            return;
        }
        if let Some(index) = ids.iter().position(|item| item.as_ref() == id) {
            if !taken[index] {
                taken[index] = true;
                out.push(index);
            }
        }
    };
    push_id(creator);
    for admin in admins {
        push_id(admin.as_ref());
    }
    for index in 0..ids.len() {
        if !taken[index] {
            out.push(index);
        }
    }
    out
}

#[must_use]
pub(crate) fn collage_picks(
    ids: &[impl AsRef<str>],
    total: u32,
    me: Option<&str>,
    creator: &str,
    admins: &[impl AsRef<str>],
) -> Vec<CollagePick> {
    if ids.is_empty() || total == 0 {
        return Vec::new();
    }
    // `topMember` is this sorted list before `m()` runs. `h()` then moves the
    // creator to the front; that is a no-op once the list is already sorted.
    let sorted = sorted_member_indexes(ids, creator, admins);
    let cap = if total > 4 { 3 } else { 4 };
    let mut faces = Vec::new();
    if total > 4 {
        // A short `topMember` keeps that order, skips nobody, and pads the
        // default glyph up to 3. A longer list skips the signed-in user unless
        // they are the creator. The widget then appends the numeric counter.
        let creator_is_me = !creator.is_empty() && me == Some(creator);
        if sorted.len() <= cap {
            for index in &sorted {
                faces.push(CollagePick::Face(*index));
            }
            while faces.len() < cap {
                faces.push(CollagePick::Placeholder);
            }
        } else {
            let mut window = cap.min(total as usize).min(sorted.len());
            let mut index = 0;
            while index < window && index < sorted.len() {
                let member = sorted[index];
                let skip = !creator_is_me && me == Some(ids[member].as_ref());
                if skip {
                    if window < sorted.len() {
                        window += 1;
                    }
                } else {
                    faces.push(CollagePick::Face(member));
                }
                index += 1;
            }
        }
        if faces.len() == 1 {
            faces.push(CollagePick::Count(1));
        }
        faces.push(CollagePick::Count(total));
    } else if (total as usize) > sorted.len() {
        for index in &sorted {
            faces.push(CollagePick::Face(*index));
        }
        while (faces.len() as u32) < total {
            faces.push(CollagePick::Placeholder);
        }
        if faces.len() == 1 {
            faces.push(CollagePick::Count(1));
        }
    } else {
        for index in sorted.iter().take(cap) {
            faces.push(CollagePick::Face(*index));
        }
        if faces.len() == 1 {
            faces.push(CollagePick::Count(1));
        }
    }
    faces
}

/// `true` when the URL is a photo Electron would paint, not a `/default` avatar.
#[must_use]
pub(crate) fn is_real_avatar(url: &str) -> bool {
    let url = url.trim();
    (url.starts_with("https://") || url.starts_with("http://")) && !url.contains("/default")
}

/// Electron `le.b`: one word keeps its first character; two or more words
/// become the first character of the first word and of the last word.
#[must_use]
pub(crate) fn avatar_initials(name: &str) -> String {
    let parts: Vec<&str> = name.split_whitespace().filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.chars().next().unwrap_or('?').to_string(),
        [first, .., last] => {
            let a = first.chars().next().unwrap_or('?');
            let b = last.chars().next().unwrap_or('?');
            format!("{}{}", upper_char(a), upper_char(b))
        }
    }
}

fn upper_char(c: char) -> String {
    c.to_uppercase().collect()
}

/// First colour of Electron's nine avatar gradients. Index is the last four
/// digits of `userId` modulo 9 (`YfZ7` / `fLyd`).
#[must_use]
pub(crate) fn avatar_color(user_id: &str) -> u32 {
    const FILLS: [u32; 9] = [
        0x0066ff, 0x0fc3ff, 0x37b361, 0xeb5e00, 0x5a2db4, 0x6f3fcf, 0xf31bc8, 0x0066ff, 0x0066ff,
    ];
    let tail: String = user_id.chars().rev().take(4).collect::<String>().chars().rev().collect();
    let n = tail.parse::<u32>().unwrap_or(0);
    FILLS[(n as usize) % FILLS.len()]
}

/// Coloured disc used for a person, and for a collage cell without a photo.
pub(crate) fn letter_disc(text: &str, fill: u32, size: f32) -> impl IntoElement {
    div()
        .size(px(size))
        .rounded(px(size / 2.0))
        .bg(rgb(fill))
        .border_1()
        .border_color(hsla(0., 0., 0., 0.1))
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .text_color(rgb(0xffffff))
        .text_size(px((size * 0.36).max(10.)))
        .font_family(FAMILY_SEMIBOLD)
        .child(text.to_owned())
}

/// Overlapping member circles. Diameters are Electron's `.zavatar-multi`
/// table: list and header are `.zavatar-l` (48), the info panel is
/// `.zavatar-xl` (56, 2 px border). The conversation list itself is size `l`,
/// not `m`.
pub(crate) fn group_collage(faces: &[CollageFace], size: f32) -> impl IntoElement {
    let n = faces.len().min(4);
    let (diameter, border, pos) = collage_layout(n, size);
    let order: Vec<usize> = (0..n).rev().collect();
    div()
        .relative()
        .size(px(size))
        .rounded(px(size / 2.0))
        .overflow_hidden()
        .children(order.into_iter().map(|i| {
            let (x, y) = pos[i];
            collage_cell(&faces[i], diameter, border, x, y)
        }))
}

/// `(diameter, border, positions)` for 2, 3, or 4 cells.
fn collage_layout(n: usize, size: f32) -> (f32, f32, Vec<(f32, f32)>) {
    // CSS widths are border-box. `m` is 40 and is used by search and label
    // rows, not by `.conversationList__avatar`.
    let (two, three, four, border) = match size.round() as i32 {
        40 => (26.0, 22.0, 22.0, 1.0),
        56 => (36.0, 32.0, 30.0, 2.0),
        _ => {
            let scale = if (size - 48.0).abs() < 0.5 {
                1.0
            } else {
                size / 48.0
            };
            (32.0 * scale, 28.0 * scale, 26.0 * scale, 1.0)
        }
    };
    let diameter = match n {
        0 | 1 => size,
        2 => two,
        3 => three,
        _ => four,
    };
    let pos = match n {
        2 => vec![(0.0, size - diameter), (size - diameter, 0.0)],
        3 => vec![
            ((size - diameter) / 2.0, 0.0),
            (0.0, size - diameter),
            (size - diameter, size - diameter),
        ],
        _ => vec![
            (0.0, 0.0),
            (size - diameter, 0.0),
            (0.0, size - diameter),
            (size - diameter, size - diameter),
        ],
    };
    (diameter, border, pos)
}

fn collage_cell(face: &CollageFace, diameter: f32, border: f32, x: f32, y: f32) -> impl IntoElement {
    let text_px = (diameter * 0.42).max(9.0);
    // GPUI clips `overflow_hidden` to a rectangle. A radius on the parent does
    // not round a child photo. The radius has to be on the image, as it is for
    // a single avatar (`img.rounded`).
    let radius = px(diameter / 2.0);
    let inner = match face {
        CollageFace::Photo(image) => gpui::img(image.clone())
            .size_full()
            .rounded(radius)
            .object_fit(gpui::ObjectFit::Cover)
            .into_any_element(),
        CollageFace::Letter { text, fill } => div()
            .size_full()
            .rounded(radius)
            .bg(rgb(*fill))
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(0xffffff))
            .text_size(px(text_px))
            .font_family(FAMILY_SEMIBOLD)
            .child(text.clone())
            .into_any_element(),
        CollageFace::Count(n) => {
            let label = if *n > 99 {
                "99+".to_owned()
            } else {
                n.to_string()
            };
            div()
                .size_full()
                .rounded(radius)
                .bg(rgb(token_rgb("--layer-background-subtle", "#eef0f3")))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(token_rgb("--text-primary", "#081b3a")))
                .text_size(px(text_px))
                .font_family(FAMILY_SEMIBOLD)
                .child(label)
                .into_any_element()
        }
        CollageFace::Placeholder => div()
            .size_full()
            .rounded(radius)
            .bg(rgb(token_rgb("--layer-background-subtle", "#eef0f3")))
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(token_rgb("--text-secondary", "#8a94a6")))
            .child(fonts::icon_el(ic::USER, "☺", diameter * 0.55))
            .into_any_element(),
    };
    let cell = div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .size(px(diameter))
        .rounded(px(diameter / 2.0))
        .overflow_hidden();
    // `.zavatar-xl` cells use a 2 px border. `m` and `l` use 1 px.
    if border >= 1.5 {
        cell.border_2()
    } else {
        cell.border_1()
    }
    .border_color(hsla(0., 0., 0., 0.12))
    .child(inner)
}

/// Blue disc with the group-list glyph. Used when a group has no photo and
/// no member photo, so the first letter of "Nhóm" is not painted as N.
pub(crate) fn group_mark(size: f32) -> impl IntoElement {
    div()
        .size(px(size))
        .rounded(px(size / 2.0))
        .bg(rgb(0x0068ff))
        .border_1()
        .border_color(hsla(0., 0., 0., 0.1))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(0xffffff))
        .child(fonts::icon_el(ic::GROUP_LIST, "◎", size * 22.0 / 48.0))
}

/// Default avatar for an account without a cached photo. Zalo PC paints a
/// coloured gradient with the initial; colour is stable per name.
fn avatar(
    initial: &str,
    fill: u32,
    online: bool,
    group: bool,
    image: Option<std::sync::Arc<gpui::RenderImage>>,
    cloud: bool,
    member_faces: &[CollageFace],
) -> impl IntoElement {
    let paint = avatar_paint(group, image.is_some(), cloud, member_faces.len());
    div()
        .relative()
        .flex_none()
        .size(px(48.))
        .when(paint == AvatarPaint::Photo, |el| {
            el.when_some(image.clone(), |el, img| {
                el.child(
                    gpui::img(img)
                        .size(px(48.))
                        .rounded(px(24.))
                        .object_fit(gpui::ObjectFit::Cover),
                )
            })
        })
        .when(paint == AvatarPaint::Cloud, |el| el.child(cloud_avatar(48.)))
        .when(paint == AvatarPaint::Collage, |el| {
            el.child(group_collage(member_faces, 48.))
        })
        .when(paint == AvatarPaint::GroupMark, |el| el.child(group_mark(48.)))
        .when(paint == AvatarPaint::Letter, |el| {
            el.child(letter_disc(initial, fill, 48.))
        })
        .when(online, |el| {
            el.child(
                div()
                    .absolute()
                    .right_0()
                    .bottom_0()
                    .size(px(12.))
                    .rounded(px(6.))
                    .bg(rgb(0x2fcc70))
                    .border_2()
                    .border_color(rgb(token_rgb("--layer-background", "#ffffff"))),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conv_list_inner_is_344() {
        assert_eq!(zalo_design::CONV_LIST_INNER_W, 344.0);
        assert_eq!(zalo_design::CONTACT_SEARCH_H, 64.0);
        assert_eq!(zalo_design::GROUP_SEARCH_H, 32.0);
        assert_eq!(zalo_design::SEARCH_INPUT_RADIUS, 5.0);
        assert_eq!(zalo_design::CONV_ITEM_H, 78.0);
        assert_eq!(zalo_design::CONTACT_ITEM_H, 72.0);
        assert_eq!(ContactTab::Friends.label(), "Danh sách bạn bè");
        assert_eq!(ContactTab::Friends.glyph().0, ic::FRIEND_LIST);
    }

    #[test]
    fn group_without_a_photo_collages_members_and_never_paints_a_letter() {
        assert_eq!(avatar_paint(true, true, false, 4), AvatarPaint::Photo);
        assert_eq!(avatar_paint(true, false, false, 3), AvatarPaint::Collage);
        assert_eq!(avatar_paint(true, false, false, 0), AvatarPaint::GroupMark);
        assert_eq!(avatar_paint(true, false, true, 4), AvatarPaint::Cloud);
        assert_eq!(avatar_paint(false, false, false, 0), AvatarPaint::Letter);
        assert_eq!(avatar_paint(false, true, false, 0), AvatarPaint::Photo);
    }

    #[test]
    fn initials_color_and_collage_follow_electron() {
        assert_eq!(avatar_initials("Lan"), "L");
        assert_eq!(avatar_initials("Nguyễn Tuấn"), "NT");
        assert_eq!(avatar_initials("Dũng Mobile Đồng Tâm"), "DT");
        assert_eq!(avatar_initials("  "), "");
        assert!(!is_real_avatar(""));
        assert!(!is_real_avatar("https://ava.zalo.me/default/x.jpg"));
        assert!(is_real_avatar("https://s120-ava-talk.zadn.vn/a.jpg"));
        assert_eq!(avatar_color("1488852095078916019"), avatar_color("6019"));
        let ids = ["1", "2", "3", "4", "5", "6"];
        let big = collage_picks(&ids, 8, Some("1"), "", &[] as &[&str]);
        assert_eq!(
            big,
            vec![
                CollagePick::Face(1),
                CollagePick::Face(2),
                CollagePick::Face(3),
                CollagePick::Count(8),
            ]
        );
        // Creator 4, then admin 3, then skip self (index 0), then index 1.
        let ordered = collage_picks(&ids, 8, Some("1"), "4", &["3"]);
        assert_eq!(
            ordered,
            vec![
                CollagePick::Face(3),
                CollagePick::Face(2),
                CollagePick::Face(1),
                CollagePick::Count(8),
            ]
        );
        // The creator is the signed-in user, so self stays in front.
        let mine = collage_picks(&ids, 8, Some("1"), "1", &[] as &[&str]);
        assert_eq!(
            mine,
            vec![
                CollagePick::Face(0),
                CollagePick::Face(1),
                CollagePick::Face(2),
                CollagePick::Count(8),
            ]
        );
        // Short topMember: original sorted order, no skip, pad to 3, then count.
        let short = collage_picks(&["1", "2"], 8, Some("1"), "2", &[] as &[&str]);
        assert_eq!(
            short,
            vec![
                CollagePick::Face(1),
                CollagePick::Face(0),
                CollagePick::Placeholder,
                CollagePick::Count(8),
            ]
        );
        assert_eq!(
            collage_picks(&["a", "b"], 2, None, "", &[] as &[&str]),
            vec![CollagePick::Face(0), CollagePick::Face(1)]
        );
        assert_eq!(
            collage_picks(&["only"], 1, None, "", &[] as &[&str]),
            vec![CollagePick::Face(0), CollagePick::Count(1)]
        );
        assert_eq!(
            collage_picks(&["a"], 3, None, "", &[] as &[&str]),
            vec![
                CollagePick::Face(0),
                CollagePick::Placeholder,
                CollagePick::Placeholder,
            ]
        );
        let (d2, b2, _) = collage_layout(2, 48.0);
        let (d3, _, _) = collage_layout(3, 48.0);
        let (d4, _, _) = collage_layout(4, 48.0);
        assert_eq!((d2, b2, d3, d4), (32.0, 1.0, 28.0, 26.0));
        let (x2, xb, _) = collage_layout(2, 56.0);
        let (x3, _, _) = collage_layout(3, 56.0);
        let (x4, _, _) = collage_layout(4, 56.0);
        assert_eq!((x2, xb, x3, x4), (36.0, 2.0, 32.0, 30.0));
        let (m2, _, _) = collage_layout(2, 40.0);
        let (m4, _, _) = collage_layout(4, 40.0);
        assert_eq!((m2, m4), (26.0, 22.0));
    }
}
