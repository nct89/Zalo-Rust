//! Chat column: header 68, empty `#ebecf0` body, composer tools 40 + input 52.

use gpui::{
    div, hsla, img, prelude::*, px, rgb, ClickEvent, Context, Focusable, Font, FontFallbacks,
    FontFeatures, FontStyle, FontWeight, InteractiveElement, IntoElement, ObjectFit, ParentElement,
    SharedString, StatefulInteractiveElement, Styled,
};

use crate::color::{str_vi, token_rgb};
use crate::context_menu::MenuKind;
use crate::data::{self, ChatConv, ChatMsg};
use crate::fonts::{self, ic, FAMILY_SEMIBOLD, FAMILY_TEXT};
use crate::media::{self, MsgKind};
use crate::rail::Rail;
use crate::shell::ShellView;
use crate::widgets::tip;
use crate::window_frame::CsdCorners;

/// Clipboard pastes are stored as `zalo-gpui-paste-<ms>.<ext>`. The chip shows
/// the picture, not that temporary name.
#[must_use]
pub(crate) fn pending_chip_label(file_name: &str) -> String {
    if file_name.starts_with("zalo-gpui-paste-") {
        "Ảnh".to_owned()
    } else {
        file_name.to_owned()
    }
}

/// Electron header: count plus `STR_PHOTO_LOWER` / `STR_PHOTOS_LOWER` (`ảnh$0$`).
/// `$0$` is a comma only when files are attached too. This tray is photos only.
#[must_use]
pub(crate) fn preview_count_label(n: usize) -> String {
    let key = if n == 1 {
        "STR_PHOTO_LOWER"
    } else {
        "STR_PHOTOS_LOWER"
    };
    let word = str_vi(key, "ảnh").replace("$0$", "");
    format!("{n} {word}")
}

/// Saved wallpaper is used only when the user picked a file on this machine.
#[must_use]
pub(crate) fn wallpaper_file<'a>(mode: &str, path: &'a str) -> Option<&'a str> {
    if mode == "file" && !path.is_empty() {
        Some(path)
    } else {
        None
    }
}

#[cfg(test)]
mod wallpaper_tests {
    use super::{pending_chip_label, preview_count_label, wallpaper_file};

    #[test]
    fn only_a_picked_file_is_a_wallpaper() {
        assert_eq!(wallpaper_file("file", "/tmp/a.png"), Some("/tmp/a.png"));
        assert_eq!(wallpaper_file("file", ""), None);
        assert_eq!(wallpaper_file("none", "/tmp/a.png"), None);
        assert_eq!(wallpaper_file("paper", "/tmp/a.png"), None);
        assert_eq!(wallpaper_file("avatar", "/tmp/a.png"), None);
        assert_eq!(wallpaper_file("mist", ""), None);
        assert_eq!(
            pending_chip_label("zalo-gpui-paste-12.png"),
            "Ảnh"
        );
        assert_eq!(pending_chip_label("hop-dong.pdf"), "hop-dong.pdf");
        assert_eq!(preview_count_label(1), "1 ảnh");
        assert_eq!(preview_count_label(2), "2 ảnh");
        assert_eq!(wallpaper_file("ink", ""), None);
    }
}

/// `#chatInput` 51 = border 1 + padding 10 + 30 + 10 (live DOM).
pub const COMPOSER_INPUT_H: f32 = 51.0;
/// `#chat-box-bar-id` 41 = border 1 + 40, buttons 32×32 at +5.
pub const COMPOSER_BAR_H: f32 = 41.0;

/// Outgoing receipt under the last bubble we sent.
struct OutgoingStatus {
    /// "Sent", "Received", or "Seen".
    label: &'static str,
    /// Members who saw it (`uid`, display name).
    seen: Vec<(String, String)>,
    /// Members who received it and have not seen it.
    delivered: Vec<(String, String)>,
    /// Every other member has seen it (`count + 1 == total`).
    all_seen: bool,
}

impl ShellView {
    /// Right of the 344 px list: header + body + composer, or a local page.
    pub(crate) fn chat_column(
        &self,
        corners: CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match self.rail {
            Rail::Settings => self
                .thread(self.selected_conv(), corners, cx)
                .into_any_element(),
            Rail::Tools => placeholder(
                str_vi("STR_TOOLS", "Công cụ"),
                "Trang cục bộ — chưa nối API",
                corners,
            )
            .into_any_element(),
            Rail::Cloud => placeholder(
                str_vi("STR_LEFT_SIDEBAR_ZALO_CLOUD", "Zalo Cloud"),
                str_vi(
                    "STR_LEFT_SIDEBAR_ZCLOUD_TOAST",
                    "Tính năng quản lý dữ liệu trên zCloud tạm thời chưa hỗ trợ trên máy tính.",
                ),
                corners,
            )
            .into_any_element(),
            Rail::Contacts => self.contacts_page(corners, cx).into_any_element(),
            Rail::Chats | Rail::Docs => self
                .thread(self.selected_conv(), corners, cx)
                .into_any_element(),
        }
    }

    fn thread(
        &self,
        conv: Option<ChatConv>,
        corners: CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(conv) = conv else {
            return placeholder("Zalo", "Chọn một hội thoại để bắt đầu", corners)
                .into_any_element();
        };
        if self.solo.is_none() && self.detached.contains(&conv.id) {
            return self.detached_placeholder(&conv, cx).into_any_element();
        }
        let chat_bg = token_rgb("--layer-background-CSC", "#ebecf0");
        let round_br = (!self.info_open || self.info_overlay) && !self.msg_search_open;
        corners
            .apply(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .min_w(px(0.))
                    .bg(rgb(chat_bg)),
                false,
                false,
                false,
                round_br,
            )
            .child(self.header(Some(&conv), cx))
            .child(self.stranger_banner(&conv, cx))
            .child(self.pinned_bar(&conv, cx))
            .child(self.message_view(Some(&conv), cx))
            .map(
                |el| match self.select_msgs.as_ref().filter(|(c, _)| *c == conv.id) {
                    Some((_, ids)) => el.child(self.select_bar(conv.id, ids.len(), cx)),
                    None => el.child(self.composer(Some(&conv), corners, cx)),
                },
            )
            .when(self.sticker_open, |el| {
                el.child(
                    div()
                        .absolute()
                        .left(px(8.))
                        .bottom(px(COMPOSER_INPUT_H + COMPOSER_BAR_H + 8.0))
                        .child(self.sticker_panel(cx)),
                )
            })
            .into_any_element()
    }

    /// "Opened in a separate window" while the conversation lives in its own window.
    fn detached_placeholder(&self, conv: &ChatConv, cx: &mut Context<Self>) -> impl IntoElement {
        let id = conv.id;
        let muted = token_rgb("--text-secondary", "#5a6981");
        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .items_center()
            .justify_center()
            .gap(px(16.))
            .bg(rgb(token_rgb("--layer-background-CSC", "#ebecf0")))
            .child(
                div()
                    .text_size(px(18.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child(conv.name.clone()),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(rgb(muted))
                    .child("Hội thoại đang mở tại cửa sổ riêng"),
            )
            .child(
                div()
                    .id("detached-here")
                    .px(px(20.))
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .rounded(px(3.))
                    .bg(rgb(token_rgb("--B55", "#0068ff")))
                    .text_color(rgb(0xffffff))
                    .font_family(FAMILY_SEMIBOLD)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.detached.remove(&id);
                        cx.notify();
                    }))
                    .child("Hiển thị tại đây"),
            )
    }

    /// Newest pinned message under the header (live Zalo PC "Messages" bar).
    fn stranger_banner(&self, conv: &ChatConv, cx: &mut Context<Self>) -> gpui::AnyElement {
        if !self.is_stranger(conv) {
            return div().into_any_element();
        }
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let uid = conv.thread_id.clone();
        let sent = self.is_friend_req_sent(&uid);
        div()
            .id("stranger-banner")
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(10.))
            .flex_none()
            .w_full()
            .min_h(px(38.))
            .px(px(16.))
            .py(px(6.))
            .bg(rgb(paper))
            .border_b_1()
            .border_color(crate::color::border_vanish())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(blue))
                            .child(fonts::icon_el(ic::ADD_CONTACT, "+", 16.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(13.))
                            .text_color(rgb(ink))
                            .truncate()
                            .child(format!(
                                "Gửi yêu cầu kết bạn với {} để trò chuyện thuận tiện hơn",
                                conv.name
                            )),
                    ),
            )
            .child(if sent {
                div()
                    .id("stranger-add-sent")
                    .flex_none()
                    .px(px(12.))
                    .py(px(5.))
                    .rounded(px(4.))
                    .bg(rgb(token_rgb("--layer-background-subtle", "#eef0f3")))
                    .text_color(rgb(muted))
                    .text_size(px(13.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child("Đã gửi lời mời")
                    .into_any_element()
            } else {
                div()
                    .id("stranger-add-btn")
                    .flex_none()
                    .px(px(12.))
                    .py(px(5.))
                    .rounded(px(4.))
                    .bg(rgb(token_rgb("--layer-background-selected", "#e5efff")))
                    .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#d4e5ff"))))
                    .text_color(rgb(blue))
                    .text_size(px(13.))
                    .font_family(FAMILY_SEMIBOLD)
                    .cursor_pointer()
                    .child("Gửi kết bạn")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.send_request(uid.clone(), cx);
                    }))
                    .into_any_element()
            })
            .into_any_element()
    }

    fn pinned_bar(&self, conv: &ChatConv, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.pinned_msgs.is_empty() {
            return div().into_any_element();
        }
        let Some(msg) = self
            .messages_of(conv.id)
            .iter()
            .rev()
            .find(|m| self.pinned_msgs.contains(&m.id))
            .cloned()
        else {
            return div().into_any_element();
        };
        let ink = token_rgb("--text-primary", "#081b3a");
        let who = if msg.from_me {
            "Bạn".to_owned()
        } else {
            msg.sender.clone()
        };
        let body = match msg.kind {
            MsgKind::Photo => "[Hình ảnh]".to_owned(),
            MsgKind::Doodle => "[Vẽ hình]".to_owned(),
            MsgKind::Location => "[Vị trí]".to_owned(),
            MsgKind::Ecard => "[Thiệp]".to_owned(),
            MsgKind::File => format!("[File] {}", msg.file_name),
            _ => msg.text.replace('\n', " "),
        };
        let unpin = msg.id;
        let jump = msg.clone();
        div()
            .id("pinned-bar")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .mx(px(8.))
            .mt(px(8.))
            .px(px(16.))
            .h(px(52.))
            .rounded(px(6.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_sm()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if jump.kind == MsgKind::Photo {
                    this.open_viewer(jump.clone(), window, cx);
                }
                cx.notify();
            }))
            .child(
                div()
                    .text_color(rgb(token_rgb("--B55", "#0068ff")))
                    .child(fonts::icon_el('\u{EC5B}', "💬", 20.)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child("Tin nhắn"),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(rgb(ink))
                            .truncate()
                            .child(format!("{who}: {body}")),
                    ),
            )
            .child(
                div()
                    .id("pinned-unpin")
                    .px(px(8.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .text_size(px(13.))
                    .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.pinned_msgs.remove(&unpin);
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child("Bỏ ghim"),
            )
            .into_any_element()
    }

    fn header(&self, conv: Option<&ChatConv>, cx: &mut Context<Self>) -> impl IntoElement {
        // `#header` 68, padding 0 16, avatar 48, title 18 px / 500 / lh 27,
        // subtitle 14 px lh 21 (group: members icon + "N members" | tag);
        // buttons 32×32 radius 3, icon 20 px ink, 4 px apart (live DOM).
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let line = crate::color::border_subtle();
        let members = conv.filter(|row| row.is_group).map(|row| {
            self.members_known(&row.thread_id)
                .map(|n| n as usize)
                .unwrap_or_else(|| self.members_of(row).len().max(1))
        });
        let photo = conv.and_then(|row| self.shown_photo(&row.thread_id));
        let cloud = conv.is_some_and(|row| {
            crate::shell::is_my_documents(&row.name, &row.thread_id, self.send2me_id.as_deref())
        });
        let typing_line = conv.and_then(|row| self.typing_label_for(row));
        let online = conv.is_some_and(|row| self.conv_online(row));
        let activity = conv.and_then(|row| self.presence_subtitle(row));
        let is_stranger = conv.is_some_and(|c| self.is_stranger(c));
        let label = conv.and_then(|row| {
            let lid = self.conv_label(row.id)?;
            self.labels
                .iter()
                .find(|l| l.id == lid)
                .map(|l| l.text.clone())
        });
        div()
            .id("header")
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(zalo_design::CHAT_HEADER_H))
            .px(px(16.))
            .bg(rgb(paper))
            .border_b_1()
            .border_color(line)
            .child(self.header_identity(
                conv,
                members,
                typing_line,
                photo,
                is_stranger,
                label,
                online,
                activity,
                cloud,
                ink,
                muted,
                cx,
            ))
            .child(div().flex_1().min_w(px(18.)))
            .child(self.header_actions(conv, cx))
    }

    fn header_actions(&self, conv: Option<&ChatConv>, cx: &mut Context<Self>) -> impl IntoElement {
        let is_group = conv.is_some_and(|row| row.is_group);
        let has_conv = conv.is_some();
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap(px(4.))
            .when(has_conv, |el| {
                el.when(is_group, |el| {
                    el.child(self.header_btn(
                        "hdr-add",
                        ic::HDR_ADD_MEMBER,
                        "+",
                        str_vi("STR_ADD_FRIEND_TO_GROUP", "Thêm thành viên"),
                        HeaderAction::AddMember,
                        cx,
                    ))
                })
                .when(!is_group, |el| {
                    el.child(self.header_btn(
                        "hdr-phone",
                        ic::HDR_CALL,
                        "☎",
                        str_vi("STR_VOICE_CALL", "Cuộc gọi thoại"),
                        HeaderAction::Call(false),
                        cx,
                    ))
                })
                .child(self.header_btn(
                    "hdr-video",
                    ic::HDR_VIDEO,
                    "▶",
                    str_vi("STR_VIDEO_CALL", "Cuộc gọi video"),
                    HeaderAction::Call(true),
                    cx,
                ))
                .child(self.header_btn(
                    "hdr-search",
                    ic::HDR_SEARCH,
                    "⌕",
                    str_vi("STR_SEARCH_MESSAGES", "Tìm kiếm tin nhắn"),
                    HeaderAction::ToggleSearch,
                    cx,
                ))
                .child(self.header_btn(
                    "hdr-info",
                    ic::HDR_RIGHT_BAR,
                    "☰",
                    str_vi("STR_CONVERSATION_INFO", "Thông tin hội thoại"),
                    HeaderAction::ToggleInfo,
                    cx,
                ))
            })
    }

    fn header_btn(
        &self,
        id: &'static str,
        code: char,
        fallback: &'static str,
        label: &'static str,
        action: HeaderAction,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let on = (matches!(action, HeaderAction::ToggleInfo) && self.info_open)
            || (matches!(action, HeaderAction::ToggleSearch) && self.msg_search_open);
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(zalo_design::HEADER_ACTION_BTN))
            .rounded(px(3.))
            .text_color(rgb(if on {
                token_rgb("--B60", "#005ae0")
            } else {
                ink
            }))
            .when(on, |el| {
                el.bg(rgb(token_rgb("--layer-background-selected", "#e5f1ff")))
            })
            .cursor_pointer()
            .hover(|style| style.bg(rgb(hover)))
            .tooltip(move |_, cx| tip(label, cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                match action {
                    HeaderAction::ToggleInfo => {
                        let open = if this.msg_search_open {
                            true
                        } else {
                            !this.info_open
                        };
                        this.msg_search_open = false;
                        this.msg_search_popup = None;
                        this.info_open = open;
                        this.note = None;
                        if this.info_wide == Some(true) {
                            this.info_pref = open && this.selected.is_some();
                            this.prefs
                                .set("showRightSidebar", serde_json::Value::Bool(this.info_pref));
                        }
                    }
                    HeaderAction::ToggleSearch => {
                        this.msg_search_open = !this.msg_search_open;
                        this.msg_search_popup = None;
                        if this.msg_search_open {
                            this.info_overlay = false;
                            let h = this.msg_search.read(cx).focus_handle(cx);
                            window.focus(&h);
                            let q = this.msg_search_q.clone();
                            this.update_msg_search(q, cx);
                        } else {
                            this.msg_search_hits.clear();
                            this.msg_search_idx = 0;
                        }
                    }
                    HeaderAction::Call(video) => {
                        if let Some(id) = this.selected {
                            this.start_call(id, video, cx);
                        }
                    }
                    HeaderAction::AddMember => {
                        if let Some(c) = this.selected_conv() {
                            let grid = c
                                .thread_id
                                .strip_prefix("g_")
                                .unwrap_or(&c.thread_id)
                                .to_owned();
                            this.open_group_picker(Some((grid, c.name.clone())), cx);
                        }
                    }
                }
                cx.notify();
            }))
            .child(fonts::icon_el(
                code,
                fallback,
                zalo_design::HEADER_ACTION_ICON,
            ))
    }

    fn reply_bar(&self, msg: &ChatMsg, cx: &mut Context<Self>) -> impl IntoElement {
        let blue = token_rgb("--B60", "#005ae0");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
        let who = if msg.from_me {
            str_vi("STR_YOU", "Bạn").to_owned()
        } else {
            msg.sender.clone()
        };
        div()
            .id("reply-bar")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .py(px(8.))
            .border_t_1()
            .border_color(crate::color::border_subtle())
            .bg(rgb(field))
            .child(div().w(px(3.)).h(px(28.)).rounded(px(2.)).bg(rgb(blue)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(blue))
                            .child(format!("{} {}", str_vi("STR_REPLY_MSG", "Trả lời"), who)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(muted))
                            .truncate()
                            .child(msg.text.clone()),
                    ),
            )
            .child(
                div()
                    .id("reply-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(px(12.))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(token_rgb("--N30", "#d6dbe1"))))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reply_to = None;
                        cx.notify();
                    }))
                    .child("×"),
            )
    }

    fn composer(
        &self,
        conv: Option<&ChatConv>,
        corners: CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let line = crate::color::border_subtle();
        let name = conv.map(|row| row.name.as_str()).unwrap_or("Zalo");
        let sticker = format!("{} tới {name}", str_vi("STR_SEND_STICKER", "Gửi Sticker"));
        let photo = format!("{} tới {name}", str_vi("STR_SEND_PHOTO", "Gửi hình ảnh"));
        let file = format!("{} tới {name}", str_vi("STR_SEND_FILE", "Gửi File"));
        let (shot_with_key, shot_without_key) = self.shot_triggers();
        let shot_key = if self.shot_with_zalo {
            &shot_with_key
        } else {
            &shot_without_key
        };
        let shot_template = if self.shot_with_zalo {
            str_vi(
                "STR_SCREENSHOT_WITH_ZALO_0",
                "Chụp kèm với cửa sổ Zalo ($0$)",
            )
        } else {
            str_vi(
                "STR_SCREENSHOT_WITHOUT_ZALO_0",
                "Chụp KHÔNG kèm cửa sổ Zalo ($0$)",
            )
        };
        let shot_tip = shot_template.replace("$0$", &crate::shortcuts::format_trigger(shot_key));
        let conv_id = conv.map(|row| row.id);
        let reply = self
            .reply_to
            .as_ref()
            .filter(|msg| conv_id == Some(msg.conv));
        let round_br = (!self.info_open || self.info_overlay) && !self.msg_search_open;
        let draft_ready = !self.pending_photos.is_empty()
            || !self.composer.read(cx).text().trim().is_empty();
        corners
            .apply(
                div()
                    .id("composer")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .bg(rgb(paper)),
                false,
                false,
                false,
                round_br,
            )
            .when_some(self.quick_popup(cx), |el, pop| el.child(pop))
            .when_some(self.quick_bar(cx), |el, bar| el.child(bar))
            .when_some(self.media_strip(cx), |el, strip| el.child(strip))
            .when_some(self.mention_strip(cx), |el, strip| el.child(strip))
            .when_some(self.format_strip(cx), |el, strip| el.child(strip))
            .when_some(reply, |el, msg| el.child(self.reply_bar(msg, cx)))
            .when(self.voice_rec.is_some(), |el| {
                el.child(self.voice_record_bar(cx))
            })
            .child(
                div()
                    .id("chat-box-bar-id")
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_none()
                    .h(px(COMPOSER_BAR_H))
                    .px(px(8.))
                    .gap(px(12.))
                    .border_t_1()
                    .border_color(line)
                    .child(self.tool_btn(
                        "cmp-sticker",
                        ic::TB_STICKER,
                        "☺",
                        sticker,
                        ToolAction::Sticker,
                        cx,
                    ))
                    .child(self.tool_btn(
                        "cmp-photo",
                        ic::TB_PHOTO,
                        "▣",
                        photo,
                        ToolAction::Photo,
                        cx,
                    ))
                    .child(self.tool_btn(
                        "cmp-file",
                        ic::TB_ATTACH,
                        "📎",
                        file,
                        ToolAction::File,
                        cx,
                    ))
                    .child(self.tool_btn(
                        "cmp-card",
                        ic::TB_NAMECARD,
                        "▤",
                        str_vi("STR_SEND_CONTACT", "Gửi danh thiếp").to_owned(),
                        ToolAction::Namecard,
                        cx,
                    ))
                    .child(self.tool_btn(
                        "cmp-shot",
                        if self.shot_with_zalo {
                            ic::TB_SHOT
                        } else {
                            ic::TB_SHOT_PLAIN
                        },
                        "▢",
                        shot_tip,
                        ToolAction::Screenshot,
                        cx,
                    ))
                    .child(
                        self.tool_btn(
                            "cmp-shot-mode",
                            ic::TB_CHEVRON,
                            "⌄",
                            "Chọn cách chụp màn hình".to_owned(),
                            ToolAction::ScreenshotMenu,
                            cx,
                        )
                        .w(px(16.))
                        .ml(px(-12.)),
                    )
                    .child(self.tool_btn(
                        "cmp-fmt",
                        ic::TB_FORMAT,
                        "A",
                        "Định dạng & Độ ưu tiên (Ctrl+Shift+X)".to_owned(),
                        ToolAction::Format,
                        cx,
                    ))
                    .child(self.tool_btn(
                        "cmp-qr",
                        ic::TB_QUICK,
                        "⚡",
                        str_vi("STR_QR_INSERT", "Chèn tin nhắn nhanh").to_owned(),
                        ToolAction::QuickReply,
                        cx,
                    ))
                    .child(self.tool_btn(
                        "cmp-bank",
                        ic::TB_BANK,
                        "▭",
                        "Gửi tài khoản ngân hàng".to_owned(),
                        ToolAction::BankCard,
                        cx,
                    ))
                    .child(self.tool_btn(
                        "cmp-more",
                        ic::CONV_MORE,
                        "…",
                        str_vi("STR_MORE_OPTION_TIP", "Tùy chọn thêm").to_owned(),
                        ToolAction::More,
                        cx,
                    )),
            )
            .when_some(self.composer_link.clone(), |el, url| {
                el.child(self.composer_link_bar(&url, cx))
            })
            .child(
                div()
                    .id("chatInput")
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_none()
                    .h(px(COMPOSER_INPUT_H))
                    .px(px(12.))
                    .border_t_1()
                    .border_color(line)
                    .child(
                        div()
                            .id("composer-input")
                            .flex_1()
                            .min_w(px(0.))
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                                    this.input_has_sel = this.composer.read(cx).has_selection();
                                    this.open_menu(MenuKind::Input, e.position, window);
                                    cx.notify();
                                }),
                            )
                            .text_size(px(zalo_design::RICH_INPUT_PX))
                            .line_height(px(22.5))
                            // Typed text is `--text-primary`; the placeholder
                            // keeps its own grey inside LineInput.
                            .text_color(rgb(crate::color::token_rgb("--text-primary", "#081b3a")))
                            .child(self.composer.clone()),
                    )
                    .child(
                        self.input_btn(
                            "cmp-emoji",
                            fonts::icon_el(ic::TB_EMOJI, "☺", 18.).into_any_element(),
                            "Biểu cảm",
                            cx,
                            ComposerSide::Emoji,
                        )
                        .ml(px(4.)),
                    )
                    .child(
                        self.input_btn(
                            if draft_ready { "cmp-send" } else { "cmp-like" },
                            if draft_ready {
                                fonts::fa_icon(
                                    fonts::FA_PAPER_PLANE,
                                    "➤",
                                    18.,
                                    token_rgb("--B55", "#0068ff"),
                                )
                                .into_any_element()
                            } else {
                                like_el(20.)
                            },
                            if draft_ready {
                                str_vi("STR_SEND", "Gửi")
                            } else {
                                "Gửi nhanh biểu tượng cảm xúc"
                            },
                            cx,
                            if draft_ready {
                                ComposerSide::Send
                            } else {
                                ComposerSide::Like
                            },
                        )
                        .ml(px(6.)),
                    ),
            )
            .when(!self.pending_photos.is_empty(), |el| {
                el.child(self.photo_tray(cx))
            })
    }

    /// `.chat-input-link-preview`: 64 px thumb, title, description, site, close.
    fn composer_link_bar(&self, url: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
        let preview = self
            .link_previews
            .get(url)
            .cloned()
            .filter(|p| !p.is_empty());
        let thumb = preview
            .as_ref()
            .and_then(|p| self.link_thumbs.get(&p.thumb).cloned());
        let title = preview
            .as_ref()
            .map(|p| p.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| media::host_of(url).to_owned());
        let desc = preview.as_ref().map(|p| p.desc.clone()).unwrap_or_default();
        let src = preview
            .as_ref()
            .map(|p| p.src.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| media::host_of(url).to_owned());
        div()
            .id("linkPreview")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(crate::color::border_subtle())
            .bg(rgb(field))
            .child(
                div()
                    .size(px(64.))
                    .flex_none()
                    .rounded(px(4.))
                    .overflow_hidden()
                    .bg(rgb(token_rgb("--layer-background", "#ffffff")))
                    .when_some(thumb, |el, image| {
                        el.child(img(image).size(px(64.)).object_fit(ObjectFit::Cover))
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
                            .line_height(px(20.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .truncate()
                            .child(title),
                    )
                    .when(!desc.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .text_color(rgb(muted))
                                .truncate()
                                .child(desc),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(rgb(muted))
                            .truncate()
                            .child(src),
                    ),
            )
            .child(
                div()
                    .id("linkPreview-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(px(12.))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(token_rgb("--N30", "#d6dbe1"))))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.composer_link_hide = this.composer_link.clone();
                        this.composer_link = None;
                        cx.notify();
                    }))
                    .child("×"),
            )
    }

    /// `.input-preview-area` under `#chatInput`. Enter and the send arrow send the tray.
    fn photo_tray(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let n = self.pending_photos.len();
        let label = preview_count_label(n);
        let (count, word) = label.split_once(' ').unwrap_or(("1", "ảnh"));
        let count = count.to_owned();
        let word = word.to_owned();
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let border = rgb(token_rgb("--border", "#d9d9d9"));
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_start()
            .gap(px(8.))
            .w_full();
        for (index, thumb) in self.pending_thumbs.clone().into_iter().enumerate() {
            row = row.child(self.preview_thumb(index, thumb, cx));
        }
        row = row.child(self.preview_add(cx));
        div()
            .id("input-preview-area")
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .px(px(12.))
            .pb(px(12.))
            .border_t_1()
            .border_color(border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .mt(px(8.))
                    .mb(px(12.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(3.))
                            .min_w(px(0.))
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(rgb(ink))
                            .child(div().font_family(FAMILY_SEMIBOLD).child(count))
                            .child(word),
                    )
                    .child(
                        div()
                            .id("preview-remove-all")
                            .flex_none()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .text_color(rgb(muted))
                            .cursor_pointer()
                            .hover(|st| {
                                st.text_color(rgb(token_rgb(
                                    "--button-tertiary-primary-focus-text",
                                    "#005ae0",
                                )))
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.clear_pending_photo();
                                cx.notify();
                            }))
                            .child(str_vi("STR_REMOVE_ALL", "Xoá tất cả")),
                    ),
            )
            .child(row)
            .when(self.caption_edit.is_some(), |el| {
                el.child(self.caption_editor(cx))
            })
    }

    /// One 72 px thumb with Electron's pencil and trash at the top-right.
    fn preview_thumb(
        &self,
        index: usize,
        thumb: Option<std::sync::Arc<gpui::RenderImage>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = rgb(token_rgb("--border", "#d9d9d9"));
        let subtle = rgb(token_rgb("--layer-background-subtle", "#ebecf0"));
        let ink = token_rgb("--text-primary", "#081b3a");
        let caption = self
            .pending_captions
            .get(index)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let edit_tip = str_vi(
            "STR_INPUT_PREVIEW_EDIT_PHOTO_CAPTION",
            "Chỉnh sửa mô tả ảnh",
        );
        let del_tip = str_vi("STR_INPUT_PREVIEW_DELETE_IMAGE", "Xoá ảnh");
        let mut cell = div()
            .id(("preview-thumb", index as u64))
            .relative()
            .size(px(72.))
            .flex_none()
            .rounded(px(4.))
            .overflow_hidden()
            .border_1()
            .border_color(border)
            .bg(subtle);
        if let Some(image) = thumb {
            cell = cell.child(img(image).size(px(72.)).object_fit(ObjectFit::Cover));
        }
        if let Some(text) = caption {
            cell = cell.child(
                div()
                    .absolute()
                    .bottom(px(0.))
                    .left(px(0.))
                    .right(px(0.))
                    .px(px(4.))
                    .py(px(2.))
                    .bg(gpui::hsla(0., 0., 0., 0.55))
                    .text_size(px(11.))
                    .line_height(px(14.))
                    .text_color(rgb(0xffffff))
                    .truncate()
                    .child(text),
            );
        }
        let pill_bg = token_rgb("--layer-background", "#ffffff");
        cell.child(
            div()
                .absolute()
                .top(px(4.))
                .right(px(4.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.))
                .p(px(2.))
                .rounded(px(4.))
                .bg(rgb(pill_bg))
                .border_1()
                .border_color(crate::color::border_subtle())
                .shadow_sm()
                .child(preview_action(
                    ("preview-edit", index as u64),
                    ic::EDIT_PHOTO,
                    "✎",
                    edit_tip,
                    ink,
                    cx.listener(move |this, event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if event.standard_click() {
                            this.begin_caption_edit(index, window, cx);
                            cx.notify();
                        }
                    }),
                ))
                .child(preview_action(
                    ("preview-trash", index as u64),
                    ic::TRASH_LINE,
                    "🗑",
                    del_tip,
                    ink,
                    cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if event.standard_click() {
                            this.remove_pending_photo(index);
                            cx.notify();
                        }
                    }),
                )),
        )
    }

    /// Caption field opened by the pencil on one pending photo.
    fn caption_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let blue = token_rgb("--B55", "#0068ff");
        div()
            .id("preview-caption-edit")
            .mt(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .w_full()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h(px(32.))
                    .child(self.caption_input.clone()),
            )
            .child(
                div()
                    .id("preview-caption-done")
                    .flex_none()
                    .px(px(8.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(blue))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.commit_caption_edit(cx);
                        this.caption_edit = None;
                        cx.notify();
                    }))
                    .child("Xong"),
            )
    }

    /// `.add-more-item`: 72×72, 2 px dashed, Plus_24 at 32 px.
    fn preview_add(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let border = rgb(token_rgb("--border", "#d9d9d9"));
        let subtle = rgb(token_rgb("--layer-background-subtle", "#ebecf0"));
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let title = str_vi("STR_ADD_MORE_MEDIA_TITLE", "Đính kèm thêm ảnh/file");
        div()
            .id("preview-add")
            .size(px(72.))
            .flex_none()
            .rounded(px(4.))
            .border_2()
            .border_dashed()
            .border_color(border)
            .bg(subtle)
            .text_color(rgb(muted))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|st| st.bg(rgb(hover)))
            .tooltip(move |_, cx| tip(title, cx))
            .on_click(cx.listener(|this, _, _, cx| {
                this.pick_and_attach(true);
                cx.notify();
            }))
            .child(fonts::icon_el(ic::PLUS_24, "+", 48.))
    }

    #[allow(dead_code)]
    fn mic_btn(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let recording = self.voice_rec.is_some();
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let danger = token_rgb("--R60", "#d93025");
        let ink = token_rgb("--text-primary", "#081b3a");
        div()
            .id("cmp-mic")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(30.))
            .rounded(px(3.))
            .text_size(px(16.))
            .text_color(rgb(if recording { 0xffffff } else { ink }))
            .when(recording, |el| el.bg(rgb(danger)))
            .when(!recording, |el| el.hover(|st| st.bg(rgb(hover))))
            .cursor_pointer()
            .tooltip(move |_, cx| {
                tip(
                    if recording {
                        "Dừng và gửi tin nhắn thoại"
                    } else {
                        "Gửi tin nhắn thoại"
                    },
                    cx,
                )
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.start_voice_record(cx);
                cx.notify();
            }))
            .child(fonts::icon_el(
                if recording { ic::TB_STOP } else { ic::TB_MIC },
                if recording { "■" } else { "🎤" },
                18.,
            ))
    }

    fn input_btn(
        &self,
        id: &'static str,
        content: gpui::AnyElement,
        label: &'static str,
        cx: &mut Context<Self>,
        side: ComposerSide,
    ) -> gpui::Stateful<gpui::Div> {
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(30.))
            .rounded(px(3.))
            .text_color(rgb(token_rgb("--icon-primary", "#081b3a")))
            .cursor_pointer()
            .hover(|st| st.bg(rgb(hover)))
            .tooltip(move |_, cx| tip(label, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                match side {
                    ComposerSide::Like => this.send_draft("👍", cx),
                    ComposerSide::Send => {
                        let text = this.composer.read(cx).text();
                        this.send_draft(&text, cx);
                        this.composer.update(cx, |input, cx| input.clear(cx));
                    }
                    ComposerSide::Emoji => {
                        this.sticker_open = !this.sticker_open;
                        this.sticker_tab = crate::stickers::StickerTab::Emoji;
                        if this.sticker_open {
                            this.ensure_sticker_data(cx);
                        }
                    }
                }
                cx.notify();
            }))
            .child(content)
    }

    fn tool_btn(
        &self,
        id: &'static str,
        code: char,
        fallback: &'static str,
        label: String,
        action: ToolAction,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let ink = token_rgb("--icon-primary", "#081b3a");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let tip_label = label.clone();
        div()
            .id(id)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .flex_none()
            .size(px(32.))
            .rounded(px(3.))
            .text_color(rgb(ink))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(hover)))
            .tooltip(move |_, cx| tip(tip_label.clone(), cx))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                match action {
                    ToolAction::Sticker => {
                        this.quick_menu = false;
                        this.sticker_open = !this.sticker_open;
                        this.note = None;
                        if this.sticker_open {
                            this.ensure_sticker_data(cx);
                        }
                    }
                    ToolAction::QuickReply => {
                        this.sticker_open = false;
                        this.quick_menu = !this.quick_menu;
                        this.note = None;
                    }
                    ToolAction::Screenshot => {
                        this.capture_screenshot(Some(window.window_handle()), cx);
                    }
                    ToolAction::ScreenshotMenu => {
                        this.open_menu(
                            crate::context_menu::MenuKind::Screenshot,
                            event.position(),
                            window,
                        );
                    }
                    ToolAction::Photo => this.pick_and_attach(true),
                    ToolAction::File => this.pick_and_attach(false),
                    ToolAction::Namecard => {
                        this.sticker_open = false;
                        this.quick_menu = false;
                        this.note = None;
                        this.open_namecard_picker(cx);
                    }
                    ToolAction::Format => {
                        this.sticker_open = false;
                        this.quick_menu = false;
                        this.note = None;
                        this.format_open = !this.format_open;
                    }
                    ToolAction::BankCard => {
                        this.sticker_open = false;
                        this.quick_menu = false;
                        this.note = None;
                        this.open_bank_card_dialog(cx);
                    }
                    ToolAction::More => {
                        this.sticker_open = false;
                        this.quick_menu = false;
                        this.note = None;
                        this.open_menu(
                            crate::context_menu::MenuKind::ComposerMore,
                            event.position(),
                            window,
                        );
                    }
                }
                cx.notify();
            }))
            .child(fonts::icon_el(code, fallback, 18.))
    }
}

#[derive(Clone, Copy)]
enum ComposerSide {
    Emoji,
    Like,
    Send,
}

#[derive(Clone, Copy)]
enum ToolAction {
    Sticker,
    QuickReply,
    Screenshot,
    /// Chevron: pick with / without the Zalo window. Does not capture.
    ScreenshotMenu,
    Photo,
    File,
    Namecard,
    Format,
    BankCard,
    More,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextStyleKind {
    Bold,
    Italic,
    Mono,
    Strike,
}

/// Parse trailing `@query` at the end of `draft` when `mentionSuggest` should propose members.
fn mention_query(draft: &str) -> Option<(usize, &str)> {
    let at_idx = draft.rfind('@')?;
    if at_idx > 0 {
        let prev = draft[..at_idx].chars().next_back()?;
        if !prev.is_whitespace() {
            return None;
        }
    }
    let query = &draft[at_idx + 1..];
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    let q_lower = query.to_ascii_lowercase();
    if q_lower == "gif"
        || q_lower == "sticker"
        || q_lower.starts_with("gif:")
        || q_lower.starts_with("sticker:")
    {
        return None;
    }
    Some((at_idx, query))
}

fn normalize_math_char(ch: char) -> char {
    let code = ch as u32;
    let mapped = match ch {
        '\u{210E}' => Some(b'h'),
        '𝐀'..='𝐙' => Some(b'A' + (code - 0x1D400) as u8),
        '𝐚'..='𝐳' => Some(b'a' + (code - 0x1D41A) as u8),
        '𝐴'..='𝑍' => Some(b'A' + (code - 0x1D434) as u8),
        '𝑎'..='𝑔' => Some(b'a' + (code - 0x1D44E) as u8),
        '𝑖'..='𝑧' => Some(b'a' + (code - 0x1D44E) as u8),
        '𝙰'..='𝚉' => Some(b'A' + (code - 0x1D670) as u8),
        '𝚊'..='𝚣' => Some(b'a' + (code - 0x1D68A) as u8),
        '𝟎'..='𝟗' => Some(b'0' + (code - 0x1D7CE) as u8),
        '𝟶'..='𝟿' => Some(b'0' + (code - 0x1D7F6) as u8),
        _ => None,
    };
    mapped.map(char::from).unwrap_or(ch)
}

fn style_math_char(plain: char, style: TextStyleKind) -> char {
    let code = match (style, plain) {
        (TextStyleKind::Bold, 'A'..='Z') => 0x1D400 + (plain as u32 - 'A' as u32),
        (TextStyleKind::Bold, 'a'..='z') => 0x1D41A + (plain as u32 - 'a' as u32),
        (TextStyleKind::Bold, '0'..='9') => 0x1D7CE + (plain as u32 - '0' as u32),
        (TextStyleKind::Italic, 'A'..='Z') => 0x1D434 + (plain as u32 - 'A' as u32),
        (TextStyleKind::Italic, 'h') => 0x210E,
        (TextStyleKind::Italic, 'a'..='z') => 0x1D44E + (plain as u32 - 'a' as u32),
        (TextStyleKind::Mono, 'A'..='Z') => 0x1D670 + (plain as u32 - 'A' as u32),
        (TextStyleKind::Mono, 'a'..='z') => 0x1D68A + (plain as u32 - 'a' as u32),
        (TextStyleKind::Mono, '0'..='9') => 0x1D7F6 + (plain as u32 - '0' as u32),
        _ => return plain,
    };
    char::from_u32(code).unwrap_or(plain)
}

/// Apply or toggle Unicode formatting (`Bold`/`Italic`/`Mono`/`Strike`) on composer text.
fn format_unicode_style(text: &str, style: TextStyleKind) -> String {
    if style == TextStyleKind::Strike {
        if text.contains('\u{0336}') {
            return text.chars().filter(|&c| c != '\u{0336}').collect();
        }
        let mut out = String::with_capacity(text.len() * 2);
        for ch in text.chars() {
            out.push(ch);
            if ch != '\n' && ch != '\r' {
                out.push('\u{0336}');
            }
        }
        return out;
    }
    let has_mappable = text.chars().any(|ch| {
        let plain = normalize_math_char(ch);
        style_math_char(plain, style) != plain
    });
    let already_styled = has_mappable
        && text.chars().all(|ch| {
            let plain = normalize_math_char(ch);
            let styled = style_math_char(plain, style);
            styled == plain || ch == styled
        });
    text.chars()
        .map(|ch| {
            let plain = normalize_math_char(ch);
            if already_styled {
                plain
            } else {
                style_math_char(plain, style)
            }
        })
        .collect()
}

impl ShellView {
    /// Text-formatting + message urgency bar toggled by `#cmp-fmt`.
    fn format_strip(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.format_open {
            return None;
        }
        let line = crate::color::border_subtle();
        let paper = token_rgb("--layer-background", "#ffffff");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let ink = token_rgb("--text-primary", "#081c36");
        let muted = token_rgb("--text-secondary", "#44546f");
        let styles = [
            (
                TextStyleKind::Bold,
                "format-bold",
                "B",
                "Đậm (chữ/vùng chọn)",
            ),
            (TextStyleKind::Italic, "format-italic", "I", "Nghiêng"),
            (TextStyleKind::Mono, "format-mono", "</>", "Đơn cách (Mono)"),
            (TextStyleKind::Strike, "format-strike", "S", "Gạch ngang"),
        ];
        let priorities = [
            (
                0_u8,
                "prio-normal",
                "Bình thường",
                token_rgb("--B55", "#0068ff"),
            ),
            (1_u8, "prio-important", "Quan trọng", 0xd9_77_06),
            (2_u8, "prio-urgent", "Khẩn cấp", 0xdc_26_26),
        ];
        Some(
            div()
                .id("format-strip")
                .flex()
                .flex_row()
                .items_center()
                .flex_none()
                .h(px(36.))
                .px(px(12.))
                .gap(px(6.))
                .bg(rgb(paper))
                .border_t_1()
                .border_color(line)
                .children(styles.into_iter().map(|(kind, id, label, hint)| {
                    div()
                        .id(id)
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(24.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .bg(rgb(hover))
                        .hover(|st| st.bg(rgb(token_rgb("--layer-background-selected", "#e2e8f0"))))
                        .tooltip(move |_, cx| tip(hint, cx))
                        .text_size(px(13.))
                        .text_color(rgb(ink))
                        .child(label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.composer.update(cx, |input, cx| {
                                input.transform_text(cx, |s| format_unicode_style(s, kind));
                            });
                            let handle = gpui::Focusable::focus_handle(this.composer.read(cx), cx);
                            window.focus(&handle);
                            cx.notify();
                        }))
                }))
                .child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(line))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(muted))
                        .mr(px(2.))
                        .child("Ưu tiên:"),
                )
                .children(priorities.into_iter().map(|(lvl, id, label, accent)| {
                    let active = self.urgency == lvl;
                    div()
                        .id(id)
                        .flex()
                        .items_center()
                        .h(px(24.))
                        .px(px(8.))
                        .rounded(px(12.))
                        .cursor_pointer()
                        .bg(rgb(if active { accent } else { hover }))
                        .text_size(px(12.))
                        .text_color(rgb(if active { 0xff_ffff } else { ink }))
                        .child(label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.urgency = lvl;
                            let handle = gpui::Focusable::focus_handle(this.composer.read(cx), cx);
                            window.focus(&handle);
                            cx.notify();
                        }))
                }))
                .into_any_element(),
        )
    }

    fn mention_candidates(
        &self,
        conv: &ChatConv,
        query: &str,
    ) -> Vec<(String, Option<std::sync::Arc<gpui::RenderImage>>)> {
        let q_lower = query.to_lowercase();
        let mut seen = std::collections::HashSet::<String>::new();
        let mut out = Vec::new();
        let mut push = |name: String, avatar: Option<std::sync::Arc<gpui::RenderImage>>| {
            let trimmed = name.trim();
            if trimmed.is_empty() || out.len() >= 6 {
                return;
            }
            if !q_lower.is_empty() && !trimmed.to_lowercase().contains(&q_lower) {
                return;
            }
            if seen.insert(trimmed.to_lowercase()) {
                out.push((trimmed.to_owned(), avatar));
            }
        };

        if conv.is_group {
            push("All".to_owned(), None);
            if let Some(prof) = self.profiles.map.get(&conv.thread_id) {
                for uid in &prof.member_ids {
                    let name = self
                        .profiles
                        .map
                        .get(uid)
                        .map(|p| p.name.clone())
                        .filter(|n| !n.is_empty())
                        .or_else(|| {
                            self.friends
                                .iter()
                                .find(|f| f.uid == *uid)
                                .map(|f| f.name.clone())
                        });
                    if let Some(name) = name {
                        push(name, self.avatar_of(uid));
                    }
                }
            }
            for name in self.members_of(conv) {
                push(name, None);
            }
        } else {
            push(conv.name.clone(), self.avatar_of(&conv.thread_id));
        }
        out
    }

    /// Floating `@mention` suggestion strip when `mentionSuggest` is enabled.
    fn mention_strip(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.prefs.bool("mentionSuggest", true) {
            return None;
        }
        let draft = self.composer.read(cx).text();
        let (at_idx, query) = mention_query(&draft)?;
        let conv = self.selected_conv()?.clone();
        let candidates = self.mention_candidates(&conv, query);
        if candidates.is_empty() {
            return None;
        }
        let line = crate::color::border_subtle();
        let paper = token_rgb("--layer-background", "#ffffff");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let ink = token_rgb("--text-primary", "#081c36");
        Some(
            div()
                .id("mention-strip")
                .flex()
                .flex_row()
                .items_center()
                .flex_none()
                .h(px(38.))
                .px(px(12.))
                .gap(px(8.))
                .overflow_x_hidden()
                .bg(rgb(paper))
                .border_t_1()
                .border_color(line)
                .children(
                    candidates
                        .into_iter()
                        .enumerate()
                        .map(|(idx, (name, photo))| {
                            let draft_snap = draft.clone();
                            let label = format!("@{name}");
                            let pick_name = name.clone();
                            div()
                                .id(("mention-pick", idx as u64))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.))
                                .h(px(26.))
                                .px(px(8.))
                                .rounded(px(13.))
                                .cursor_pointer()
                                .bg(rgb(hover))
                                .hover(|st| {
                                    st.bg(rgb(token_rgb("--layer-background-selected", "#e2e8f0")))
                                })
                                .when_some(photo, |el, p| {
                                    el.child(
                                        img(p)
                                            .size(px(18.))
                                            .rounded(px(9.))
                                            .object_fit(ObjectFit::Cover),
                                    )
                                })
                                .child(
                                    div()
                                        .max_w(px(160.))
                                        .truncate()
                                        .text_size(px(13.))
                                        .text_color(rgb(ink))
                                        .child(label),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let mut next = draft_snap[..at_idx].to_owned();
                                    next.push('@');
                                    next.push_str(&pick_name);
                                    next.push(' ');
                                    this.composer.update(cx, |input, cx| {
                                        input.set_text(next.clone(), cx);
                                    });
                                    this.on_composer_media_change(&next, cx);
                                    let handle =
                                        gpui::Focusable::focus_handle(this.composer.read(cx), cx);
                                    window.focus(&handle);
                                    cx.notify();
                                }))
                        }),
                )
                .into_any_element(),
        )
    }
}

#[derive(Clone, Copy)]
enum HeaderAction {
    ToggleInfo,
    ToggleSearch,
    /// ☎ (false) / video (true).
    Call(bool),
    AddMember,
}

#[derive(Clone)]
enum MsgRowItem {
    LoadOlder(usize),
    DateChip(String),
    Bubble {
        msg_idx: usize,
        album_start: usize,
        album_end: usize,
        first: bool,
        last: bool,
        has_status_pill: bool,
    },
    BottomSpacer,
}

impl ShellView {
    fn build_msg_rows(&self, row: &ChatConv) -> (usize, Vec<MsgRowItem>) {
        let all = self.messages_of(row.id);
        let hidden = all.len().saturating_sub(self.msg_window);
        let msgs: &[ChatMsg] = &all[hidden..];
        let mut items: Vec<MsgRowItem> = Vec::with_capacity(msgs.len() + 8);
        if hidden > 0 {
            items.push(MsgRowItem::LoadOlder(hidden));
        }
        let last_own_id = msgs
            .iter()
            .rev()
            .find(|m| m.from_me && !m.recalled)
            .map(|m| m.id);
        let mut last_day: Option<i64> = None;
        let now_ms = data::now_ms();
        let mut i = 0;
        while i < msgs.len() {
            let msg = &msgs[i];
            let day = (msg.at_ms > 0).then(|| data::vn_day(msg.at_ms));
            let new_day = day != last_day;
            if new_day {
                let chip = if msg.at_ms > 0 {
                    data::day_label(msg.at_ms, now_ms)
                } else {
                    format!("{} {}", msg.time, crate::preview::thread_day(row.id))
                };
                items.push(MsgRowItem::DateChip(chip));
                last_day = day;
            }
            let album_end = data::photo_album_end(msgs, i);
            let span = if album_end > i + 1 { album_end } else { i + 1 };
            let head = &msgs[i];
            let tail = &msgs[span - 1];
            let prev = if new_day {
                None
            } else {
                i.checked_sub(1).and_then(|j| msgs.get(j))
            };
            let next = msgs.get(span);
            let first = prev.is_none_or(|p| !same_block(p, head)) || i == 0;
            let last = next.is_none_or(|n| !same_block(tail, n));
            let has_status_pill = row.live && tail.from_me && Some(tail.id) == last_own_id;
            items.push(MsgRowItem::Bubble {
                msg_idx: hidden + span - 1,
                album_start: hidden + i,
                album_end: if span > i + 1 { hidden + span } else { hidden + i },
                first,
                last,
                has_status_pill,
            });
            i = span;
        }
        if !msgs.is_empty() {
            items.push(MsgRowItem::BottomSpacer);
        }
        (hidden, items)
    }

    fn sync_msg_list(&self, conv_id: u64, next_count: usize, hidden: usize) {
        let (prev_conv, old_count, prev_hidden) = self.msg_list_state.get();
        if prev_conv != Some(conv_id) {
            self.msg_list.reset(next_count);
            self.msg_list_state.set((Some(conv_id), next_count, hidden));
        } else if old_count != next_count || prev_hidden != hidden {
            if hidden < prev_hidden && next_count >= old_count {
                // Older messages were prepended at the top ("Xem tin nhắn cũ hơn"):
                // splice the top prefix so the viewport stays anchored on the current message.
                let delta = next_count - old_count;
                let replace_top = old_count.min(1);
                self.msg_list.splice(0..replace_top, delta + replace_top);
            } else if next_count > old_count && old_count > 0 {
                // New messages appended at the bottom: splice the trailing items so
                // scroll position stays intact (and ListAlignment::Bottom stays pinned if at bottom).
                let tail_start = old_count.saturating_sub(2);
                self.msg_list
                    .splice(tail_start..old_count, next_count - tail_start);
            } else {
                self.msg_list.reset(next_count);
            }
            self.msg_list_state.set((Some(conv_id), next_count, hidden));
        }
    }

    fn message_view(&self, conv: Option<&ChatConv>, cx: &mut Context<Self>) -> impl IntoElement {
        let chat_bg = token_rgb("--layer-background-CSC", "#ebecf0");
        let Some(row) = conv else {
            return div()
                .id("message-view")
                .flex()
                .flex_1()
                .w_full()
                .bg(rgb(chat_bg))
                .into_any_element();
        };
        let (hidden, items) = self.build_msg_rows(row);
        self.sync_msg_list(row.id, items.len(), hidden);
        let items = std::rc::Rc::new(items);
        let row_owned = row.clone();
        let conv_id = row.id;
        let is_group = row.is_group;
        let wall_image = wallpaper_file(
            &self.prefs.str("chatWallpaper", ""),
            &self.prefs.str("chatWallpaperPath", ""),
        )
        .and_then(|path| self.wallpaper_image(path));
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .when(self.react_picker.is_some(), |el| {
                el.on_scroll_wheel(cx.listener(|this, _, _, cx| {
                    if this.react_picker.is_some() {
                        this.close_react_picker();
                        cx.notify();
                    }
                }))
            })
            .when_some(wall_image, |el, image| {
                el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .overflow_hidden()
                        .child(img(image).size_full().object_fit(ObjectFit::Cover)),
                )
            })
            .child(
                gpui::list(
                    self.msg_list.clone(),
                    cx.processor(move |this, ix: usize, _window, cx| {
                        let Some(item) = items.get(ix) else {
                            return div().into_any_element();
                        };
                        let inner = match item {
                            MsgRowItem::LoadOlder(hidden_n) => div()
                                .id("load-older")
                                .flex()
                                .justify_center()
                                .w_full()
                                .pt(px(8.))
                                .pb(px(4.))
                                .child(
                                    div()
                                        .id("load-older-btn")
                                        .px(px(12.))
                                        .h(px(28.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(14.))
                                        .bg(rgb(token_rgb("--layer-background", "#ffffff")))
                                        .text_size(px(13.))
                                        .text_color(rgb(token_rgb("--B55", "#0068ff")))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.msg_window += crate::shell::MSG_WINDOW;
                                            if let Some(id) = this.selected {
                                                this.warm_media(id);
                                            }
                                            cx.notify();
                                        }))
                                        .child(format!("Xem tin nhắn cũ hơn ({hidden_n})")),
                                )
                                .into_any_element(),
                            MsgRowItem::DateChip(chip) => date_chip(chip).into_any_element(),
                            MsgRowItem::Bubble {
                                msg_idx,
                                album_start,
                                album_end,
                                first,
                                last,
                                has_status_pill,
                            } => {
                                let all = this.messages_of(conv_id);
                                let Some(shown) = all.get(*msg_idx).cloned() else {
                                    return div().into_any_element();
                                };
                                let status_pill = (*has_status_pill)
                                    .then(|| this.outgoing_status(&row_owned, &shown));
                                let album: Vec<ChatMsg> =
                                    if *album_end > *album_start + 1 && *album_end <= all.len() {
                                        all[*album_start..*album_end].to_vec()
                                    } else {
                                        Vec::new()
                                    };
                                this.bubble(
                                    shown,
                                    is_group,
                                    *first,
                                    *last,
                                    status_pill,
                                    &album,
                                    cx,
                                )
                            }
                            MsgRowItem::BottomSpacer => {
                                div().h(px(48.)).w_full().flex_none().into_any_element()
                            }
                        };
                        div()
                            .w_full()
                            .px(px(16.))
                            .when(ix == 0, |el| el.pt(px(12.)))
                            .child(inner)
                            .into_any_element()
                    }),
                )
                .flex_1()
                .w_full()
                .min_h(px(0.)),
            )
            .into_any_element()
    }

    /// "Sent" / "Received" / "Seen" for the newest message we sent.
    ///
    /// Groups larger than `MAX_GROUP_SEEN_MEMBERS` stay on "Sent": Zalo PC
    /// does not upgrade that status or show the member list.
    fn outgoing_status(&self, row: &ChatConv, msg: &ChatMsg) -> OutgoingStatus {
        let hide = row.is_group
            && self
                .members_known(&row.thread_id)
                .is_some_and(|n| n > crate::read_sync::MAX_GROUP_SEEN_MEMBERS);
        let all = self.messages_of(row.id);
        let saw = !hide
            && self
                .read_sync
                .peer_saw(&msg.thread_id, msg.global_msg_id, msg.at_ms, |gid| {
                    all.iter()
                        .find(|item| item.global_msg_id == Some(gid))
                        .map(|item| item.at_ms)
                });
        let got = !hide
            && !saw
            && self
                .read_sync
                .peer_got(&msg.thread_id, msg.global_msg_id, msg.at_ms, |gid| {
                    all.iter()
                        .find(|item| item.global_msg_id == Some(gid))
                        .map(|item| item.at_ms)
                });
        let label = if saw {
            "Đã xem"
        } else if got {
            "Đã nhận"
        } else {
            "Đã gửi"
        };
        let (seen_uids, delivered_uids) = if hide || !row.is_group {
            (Vec::new(), Vec::new())
        } else {
            self.read_sync
                .member_lists(&msg.thread_id, msg.global_msg_id)
        };
        let name_of = |uid: &str| {
            let raw = self
                .profiles
                .map
                .get(uid)
                .map(|profile| profile.name.as_str())
                .unwrap_or("");
            media::sender_label(uid, self.profiles.alias_or(uid, raw))
        };
        let seen: Vec<_> = seen_uids
            .iter()
            .map(|uid| (uid.clone(), name_of(uid)))
            .collect();
        let delivered: Vec<_> = delivered_uids
            .iter()
            .map(|uid| (uid.clone(), name_of(uid)))
            .collect();
        let all_seen = self
            .members_known(&row.thread_id)
            .is_some_and(|n| n > 1 && seen.len() as u32 + 1 == n);
        OutgoingStatus {
            label,
            seen,
            delivered,
            all_seen,
        }
    }

    /// Dark pill plus, for a small group, the people who saw or received it.
    fn receipt_el(
        &self,
        msg_id: u64,
        pill: OutgoingStatus,
        on_canvas: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let faces: Vec<_> = if pill.seen.is_empty() {
            pill.delivered.iter().take(3).cloned().collect()
        } else {
            pill.seen.iter().take(3).cloned().collect()
        };
        let extra = if pill.seen.is_empty() {
            pill.delivered.len().saturating_sub(faces.len())
        } else {
            pill.seen.len().saturating_sub(faces.len())
        };
        let has_people = !pill.seen.is_empty() || !pill.delivered.is_empty();
        let open = self.receipt_open == Some(msg_id);
        let hover = if pill.all_seen {
            "Đã xem: Tất cả".to_owned()
        } else if !pill.seen.is_empty() {
            format!(
                "Đã xem: {}",
                pill.seen
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            format!(
                "Đã nhận: {}",
                pill.delivered
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let pill_bg = timestamp_pill_bg();
        div()
            .flex()
            .flex_col()
            .items_end()
            .child(
                div()
                    .id(("receipt", msg_id))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .px(px(8.))
                    .h(px(20.))
                    .rounded(px(10.))
                    .bg(pill_bg)
                    .text_size(px(11.))
                    .line_height(px(20.))
                    .text_color(rgb(0xffffff))
                    .when(has_people, |el| {
                        el.cursor_pointer()
                            .tooltip(move |_, cx| tip(hover.clone(), cx))
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                if event.standard_click() {
                                    this.receipt_open = if this.receipt_open == Some(msg_id) {
                                        None
                                    } else {
                                        Some(msg_id)
                                    };
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                            }))
                    })
                    .child(if on_canvas && pill.label == "Đã nhận" {
                        "✓✓ Đã nhận".to_owned()
                    } else {
                        pill.label.to_owned()
                    })
                    .children(faces.iter().map(|(uid, name)| self.receipt_face(uid, name)))
                    .when(extra > 0, |el| el.child(format!("+{extra}"))),
            )
            .when(open && has_people, |el| el.child(receipt_panel(&pill)))
    }

    fn receipt_face(&self, uid: &str, name: &str) -> gpui::AnyElement {
        if let Some(photo) = self.avatar_of(uid) {
            return img(photo)
                .size(px(16.))
                .rounded(px(8.))
                .object_fit(ObjectFit::Cover)
                .into_any_element();
        }
        let initial = name.chars().next().unwrap_or('?').to_string();
        div()
            .size(px(16.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(9.))
            .text_color(rgb(token_rgb("--text-primary", "#081b3a")))
            .child(initial)
            .into_any_element()
    }

    /// Decode the chosen wallpaper once and keep it for later frames.
    fn wallpaper_image(&self, path: &str) -> Option<std::sync::Arc<gpui::RenderImage>> {
        if let Some((cached, image)) = self.wall_image.borrow().as_ref() {
            if cached == path {
                return Some(std::sync::Arc::clone(image));
            }
        }
        let image = media::decode_thumb(std::path::Path::new(path), 1400)?;
        *self.wall_image.borrow_mut() = Some((path.to_owned(), std::sync::Arc::clone(&image)));
        Some(image)
    }

    /// `.chat-message`: peer avatar 40 (first of a block), card padding 12,
    /// radius 6, `--blue-message` / white; time inside the card on the last
    /// message of a block (12 px `#44546f`), or a dark pill under media;
    /// hover shows Quote / Share / More (24 px round) and the reaction button.
    fn bubble(
        &self,
        msg: ChatMsg,
        is_group: bool,
        first: bool,
        last: bool,
        status_pill: Option<OutgoingStatus>,
        album: &[ChatMsg],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let mine = token_rgb("--blue-message", "#e5f1ff");
        let theirs = token_rgb("--white-message", "#ffffff");
        // A message that is only a phone number (plus a caption) is the
        // Electron linked-number card, not a plain text bubble.
        let phone = (!msg.recalled && msg.kind == MsgKind::Text)
            .then(|| media::phone_note(&msg.text))
            .flatten();
        let fill = if phone.is_some() || !msg.from_me {
            theirs
        } else {
            mine
        };
        let row_id = msg.id;
        let liked = msg.liked;
        let starred = self.starred.contains(&msg.id);
        let pinned = self.pinned_msgs.contains(&msg.id);
        let click_msg = msg.clone();
        let media = matches!(
            msg.kind,
            MsgKind::Sticker | MsgKind::Gif | MsgKind::Photo | MsgKind::Doodle
        ) && !msg.recalled;
        let bare = matches!(msg.kind, MsgKind::Sticker | MsgKind::Gif) && !msg.recalled;
        let quote_box = self.quote_card(&msg, media, cx);
        let has_quote = quote_box.is_some();
        let body = if album.len() > 1 {
            self.album_body(album, ink, cx)
        } else {
            self.bubble_body(&msg, fill, ink, muted, cx)
        };
        let show_sender = is_group && !msg.from_me && first;
        let urgency = self.urgency_msgs.get(&row_id).copied().unwrap_or(0);
        let group = SharedString::from(format!("bubble-{row_id}"));
        // Text keeps the clock on the bubble fill. Calls, files, links and the
        // other cards put it on the chat canvas, the same split Zalo PC uses.
        let card_clock =
            last && !msg.recalled && (crate::media::clock_below_card(msg.kind) || phone.is_some());
        let time_in_card = last && !media && !card_clock;
        let search_active = self.msg_search_open
            && self.msg_search_has_filter()
            && self.msg_search_hits.contains(&row_id);
        let search_focused =
            search_active && self.msg_search_hits.get(self.msg_search_idx) == Some(&row_id);
        let border_c: gpui::Hsla = if phone.is_some() || search_focused {
            rgb(token_rgb("--B55", "#0068ff")).into()
        } else if search_active {
            rgb(0x66a6ff).into()
        } else if media && msg.from_me {
            rgb(0x66a6ff).into()
        } else {
            hsla(0., 0., 0., 0.)
        };
        let card = if bare {
            div().relative().child(body)
        } else {
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_none()
                .when(!media || has_quote, |el| el.p(px(media::BUBBLE_PAD)))
                .rounded(px(zalo_design::CARD_RADIUS))
                .bg(rgb(fill))
                .border_1()
                .border_color(border_c)
                .max_w(px(media::BUBBLE_MAX + 40.0))
                .when(phone.is_some(), |el| el.min_w(px(240.)))
                .when(urgency > 0 && !media, |el| {
                    let (label, bg_c, fg_c) = if urgency >= 2 {
                        ("Tin khẩn cấp", 0xfe_e2_e2, 0xb9_1c_1c)
                    } else {
                        ("Tin quan trọng", 0xfe_f3_c7, 0xb4_53_09)
                    };
                    el.child(
                        div()
                            .mb(px(4.))
                            .px(px(6.))
                            .h(px(18.))
                            .rounded(px(4.))
                            .bg(rgb(bg_c))
                            .text_size(px(11.))
                            .line_height(px(18.))
                            .text_color(rgb(fg_c))
                            .child(label),
                    )
                })
                .when(show_sender && !media && phone.is_none(), |el| {
                    el.child(
                        div()
                            .mb(px(6.))
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .text_color(rgb(muted))
                            .child(msg.sender.clone()),
                    )
                })
                .when_some(quote_box, |el, q| el.child(q))
                .child(body)
                .when(time_in_card, |el| {
                    el.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .mt(px(4.))
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(token_rgb("--text-secondary", "#44546f")))
                            .child(msg.time.clone())
                            .when(starred, |el| el.child("★"))
                            .when(pinned, |el| el.child(fonts::icon_el(ic::PIN, "📌", 12.))),
                    )
                })
        };
        let actions = div()
            .flex()
            .flex_col()
            .justify_end()
            .flex_none()
            .when(msg.from_me, |el| el.mr(px(8.)))
            .when(!msg.from_me, |el| el.ml(px(8.)))
            .child(self.hover_actions(&msg, &group, cx));
        let react_summary = self
            .reactions
            .get(&row_id)
            .filter(|v| !v.is_empty())
            .map(|list| {
                let mut indices: Vec<usize> = Vec::new();
                for r in list {
                    let idx = crate::read_sync::reaction_preset_index(&r.r_icon, r.r_type);
                    if !indices.contains(&idx) && indices.len() < 3 {
                        indices.push(idx);
                    }
                }
                (indices, list.len())
            });
        let has_reacts = react_summary.is_some();
        let my_reacted = self.my_reaction(row_id).is_some();
        let react_hold_msg = msg.clone();
        let react_tip = if my_reacted {
            "Nhấn bỏ cảm xúc · Giữ để đổi"
        } else {
            "Nhấn để thích · Giữ để chọn cảm xúc"
        };
        let react_content = match react_summary {
            Some((indices, total)) => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.))
                .children(indices.into_iter().map(|idx| {
                    match media::reaction_icon_by_index(idx) {
                        Some(image) => img(image)
                            .size(px(15.))
                            .object_fit(ObjectFit::Contain)
                            .into_any_element(),
                        None => div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(ink))
                            .child(crate::read_sync::REACTION_PRESETS[idx].0)
                            .into_any_element(),
                    }
                }))
                .when(total > 1, |el| {
                    el.child(
                        div()
                            .ml(px(2.))
                            .text_size(px(11.5))
                            .line_height(px(16.))
                            .text_color(rgb(ink))
                            .child(total.to_string()),
                    )
                })
                .into_any_element(),
            None => like_el(15.).into_any_element(),
        };
        let react = div()
            .id(("react", row_id))
            .absolute()
            .right(px(4.))
            .bottom(px(4.))
            .h(px(24.))
            .min_w(px(24.))
            .when(has_reacts, |el| el.px(px(6.)))
            .when(!has_reacts, |el| el.w(px(24.)))
            .rounded(px(12.))
            .bg(rgb(crate::color::token_rgb(
                "--reaction-background",
                "#ffffff",
            )))
            .shadow_sm()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(!liked && !has_reacts, |el| {
                el.invisible().group_hover(group.clone(), |st| st.visible())
            })
            .tooltip(move |_, cx| tip(react_tip, cx))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.react_hold_msg = Some(row_id);
                    this.react_hold_opened = false;
                    this.react_hold_gen = this.react_hold_gen.wrapping_add(1);
                    let gen = this.react_hold_gen;
                    let press_pos = e.position;
                    let hold_target = react_hold_msg.clone();
                    let weak = cx.weak_entity();
                    window
                        .spawn(cx, async move |cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(200))
                                .await;
                            let _ = weak.update_in(cx, |view, window, cx| {
                                if view.react_hold_gen == gen
                                    && view.react_hold_msg == Some(row_id)
                                {
                                    view.react_hold_opened = true;
                                    view.open_react_picker(hold_target, press_pos, window, true);
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let was_quick =
                        this.react_hold_msg == Some(row_id) && !this.react_hold_opened;
                    this.react_hold_gen = this.react_hold_gen.wrapping_add(1);
                    this.react_hold_msg = None;
                    if was_quick {
                        if let Some(m) = this
                            .messages
                            .get(&click_msg.conv)
                            .and_then(|l| l.iter().find(|m| m.id == click_msg.id))
                            .cloned()
                        {
                            this.apply_like(&m);
                        }
                        this.close_react_picker();
                    } else if this.react_hold_opened {
                        this.react_hold_opened = false;
                        if let Some(p) = this.react_picker.as_mut() {
                            p.holding = false;
                        }
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, _| {
                    if this.react_hold_msg == Some(row_id) && !this.react_hold_opened {
                        this.react_hold_msg = None;
                        this.react_hold_gen = this.react_hold_gen.wrapping_add(1);
                    }
                }),
            )
            .child(react_content);
        let menu_msg = msg.clone();
        let selecting = self
            .select_msgs
            .as_ref()
            .filter(|(c, _)| *c == msg.conv)
            .map(|(_, ids)| ids.contains(&row_id));
        let content = div()
            .flex()
            .flex_col()
            .flex_none()
            .min_w(px(0.))
            .when(msg.from_me, |el| el.items_end())
            .when(!msg.from_me, |el| el.items_start())
            .child(
                card.child(react)
                    .when(self.prefs.bool("peekGuard", false), |el| {
                        el.child(peek_cover(&group, fill))
                    }),
            )
            .when(last && media && status_pill.is_none(), |el| {
                el.child(
                    div()
                        .mt(px(6.))
                        .px(px(8.))
                        .h(px(20.))
                        .rounded(px(10.))
                        .bg(timestamp_pill_bg())
                        .text_size(px(11.5))
                        .line_height(px(20.))
                        .text_color(rgb(0xffffff))
                        .child(msg.time.clone()),
                )
            })
            .when(card_clock || status_pill.is_some(), |el| {
                let show_clock = card_clock || (last && media);
                el.child(
                    div()
                        .mt(px(if card_clock { 8. } else { 6. }))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .when(show_clock, |row| {
                            row.child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(4.))
                                    .px(px(8.))
                                    .h(px(20.))
                                    .rounded(px(10.))
                                    .bg(timestamp_pill_bg())
                                    .text_size(px(11.5))
                                    .line_height(px(20.))
                                    .text_color(rgb(0xffffff))
                                    .child(msg.time.clone())
                                    .when(starred, |c| c.child("★"))
                                    .when(pinned, |c| c.child(fonts::icon_el(ic::PIN, "📌", 12.))),
                            )
                        })
                        .when_some(status_pill, |row, pill| {
                            row.child(self.receipt_el(row_id, pill, true, cx))
                        }),
                )
            });
        let dbl_msg = menu_msg.clone();
        div()
            .id(("bubble-row", row_id))
            .group(group)
            .relative()
            .flex()
            .flex_row()
            .flex_none()
            .w_full()
            .pt(px(if first { 12. } else { 4. }))
            .when(selecting.is_some(), |el| el.pl(px(40.)))
            .when(msg.from_me, |el| el.justify_end().pl(px(88.)))
            .when(!msg.from_me, |el| {
                el.justify_start().pr(px(48.)).child(
                    div().flex_none().w(px(40.)).mr(px(10.)).when(first, |el| {
                        el.child(small_avatar(
                            &msg.sender,
                            &msg.sender_uid,
                            self.shown_photo(&msg.sender_uid),
                        ))
                    }),
                )
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.standard_click()
                    && event.click_count() == 2
                    && this.select_msgs.is_none()
                    && !dbl_msg.recalled
                    && this.prefs.bool("dblReply", true)
                {
                    this.reply_to = Some(dbl_msg.clone());
                    let handle = gpui::Focusable::focus_handle(this.composer.read(cx), cx);
                    window.focus(&handle);
                    cx.notify();
                }
            }))
            // GPUI 0.2.2 `on_click` only fires for the left button.
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    this.open_menu(MenuKind::Message(menu_msg.clone()), event.position, window);
                    cx.notify();
                }),
            )
            .map(|el| {
                if msg.from_me {
                    el.child(actions).child(content)
                } else {
                    el.child(content).child(actions)
                }
            })
            // Multi-select: round tick on the left, and a transparent layer
            // that turns any click on the row into a toggle.
            .when_some(selecting, |el, on| {
                let blue = token_rgb("--B55", "#0068ff");
                el.child(
                    div()
                        .absolute()
                        .left(px(8.))
                        .top_0()
                        .bottom_0()
                        .flex()
                        .items_center()
                        .text_color(if on {
                            rgb(blue)
                        } else {
                            rgb(token_rgb("--icon-secondary", "#5a6981"))
                        })
                        .child(fonts::icon_el(
                            if on { '\u{ECEF}' } else { '\u{EDD3}' },
                            "○",
                            22.,
                        )),
                )
                .child(
                    div()
                        .id(("bubble-select", row_id))
                        .absolute()
                        .inset(px(0.))
                        .occlude()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some((_, ids)) = this.select_msgs.as_mut() {
                                if let Some(i) = ids.iter().position(|x| *x == row_id) {
                                    ids.remove(i);
                                } else if ids.len() < 100 {
                                    ids.push(row_id);
                                }
                            }
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    /// Bottom bar while selecting messages: "N selected" + Share / Copy /
    /// Delete / Cancel (replaces the composer, like Zalo PC).
    fn select_bar(&self, conv: u64, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let paper = token_rgb("--layer-background", "#ffffff");
        let red = token_rgb("--R60", "#c31818");
        let picked = move |this: &Self| -> Vec<ChatMsg> {
            let ids = this
                .select_msgs
                .as_ref()
                .map(|(_, ids)| ids.clone())
                .unwrap_or_default();
            this.messages_of(conv)
                .iter()
                .filter(|m| ids.contains(&m.id))
                .cloned()
                .collect()
        };
        let btn =
            |key: &'static str, icon: char, label: &'static str, color: u32, enabled: bool| {
                div()
                    .id(key)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .h(px(36.))
                    .px(px(14.))
                    .rounded(px(4.))
                    .text_size(px(14.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(if enabled {
                        rgb(color)
                    } else {
                        rgb(token_rgb("--text-disabled", "#8b96a7"))
                    })
                    .bg(rgb(crate::color::token_rgb(
                        "--button-secondary-neutral-normal",
                        "#e5e7eb",
                    )))
                    .when(enabled, |el| {
                        el.cursor_pointer().hover(|st| {
                            st.bg(rgb(crate::color::token_rgb(
                                "--button-secondary-neutral-hover",
                                "#d6d9de",
                            )))
                        })
                    })
                    .child(fonts::icon_el(icon, "", 18.))
                    .child(crate::i18n::t(&label))
            };
        let any = count > 0;
        div()
            .flex()
            .flex_none()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .h(px(COMPOSER_INPUT_H + COMPOSER_BAR_H))
            .px(px(16.))
            .bg(rgb(paper))
            .border_t_1()
            .border_color(crate::color::border_subtle())
            .text_color(rgb(ink))
            .child(
                div()
                    .flex_1()
                    .text_size(px(15.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child(format!("Đã chọn {count}")),
            )
            .child(
                btn("sel-share", ic::MSG_SHARE, "Chia sẻ", ink, any).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let msgs = picked(this);
                        if !msgs.is_empty() {
                            this.select_msgs = None;
                            this.open_share_many(msgs, cx);
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                btn("sel-copy", '\u{EAC5}', "Copy", ink, any).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let text = picked(this)
                            .iter()
                            .map(|m| format!("{} {}: {}", m.time, m.sender, m.text))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !text.is_empty() {
                            crate::clip::copy_text(cx, text);
                            this.set_note("Đã copy tin nhắn đã chọn");
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                btn("sel-delete", '\u{EDCF}', "Xóa", red, any).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let msgs = picked(this);
                        for m in &msgs {
                            this.apply_delete_msg(m);
                        }
                        this.select_msgs = None;
                        if !msgs.is_empty() {
                            this.set_note(format!("Đã xóa {} tin nhắn ở phía bạn", msgs.len()));
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                btn("sel-cancel", '\u{EA9F}', "Hủy", ink, true).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.select_msgs = None;
                        cx.notify();
                    },
                )),
            )
    }

    /// `.MQA-contents`: Quote / Share / More, 24 px round.
    /// Chip fill is `--floating-menu` (light white 80%, dark white 10%).
    /// Icons are `--icon-secondary`. Hover fill is `--icon-hover-bg`.
    fn hover_actions(
        &self,
        msg: &ChatMsg,
        group: &SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let muted = token_rgb("--icon-secondary", "#5a6981");
        let chip = if crate::color::is_dark() {
            hsla(0., 0., 1., 0.1)
        } else {
            hsla(0., 0., 1., 0.8)
        };
        let hover = token_rgb("--icon-hover-bg", "#e5e7eb");
        let mk = |id: &'static str, code: char, label: &'static str| {
            div()
                .id((id, msg.id))
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(12.))
                .bg(chip)
                .text_color(rgb(muted))
                .cursor_pointer()
                .hover(|st| st.bg(rgb(hover)))
                .tooltip(move |_, cx| tip(label, cx))
                .child(fonts::icon_el(code, "•", 14.))
        };
        let react_btn = msg.clone();
        let reply = msg.clone();
        let share = msg.clone();
        let more = msg.clone();
        div()
            .id(("mqa", msg.id))
            .flex()
            .flex_row()
            .flex_none()
            .gap(px(4.))
            .mb(px(8.))
            .invisible()
            .group_hover(group.clone(), |st| st.visible())
            .child(mk("mqa-react", ic::TB_STICKER, "Thả cảm xúc").on_click(cx.listener(
                move |this, event: &ClickEvent, window, cx| {
                    this.open_react_picker(react_btn.clone(), event.position(), window, false);
                    cx.notify();
                },
            )))
            .child(
                mk("mqa-quote", ic::MSG_QUOTE, "Trả lời").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.reply_to = Some(reply.clone());
                        cx.notify();
                    },
                )),
            )
            .child(
                mk("mqa-share", ic::MSG_SHARE, "Chia sẻ").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.open_share(share.clone(), cx);
                        cx.notify();
                    },
                )),
            )
            .child(mk("mqa-more", ic::MSG_MORE, "Thêm").on_click(cx.listener(
                move |this, event: &ClickEvent, window, cx| {
                    this.open_menu(MenuKind::Message(more.clone()), event.position(), window);
                    cx.notify();
                },
            )))
    }

    fn quote_card(
        &self,
        msg: &ChatMsg,
        _media: bool,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if msg.recalled || (msg.quote_text.is_empty() && msg.quote_sender.is_empty()) {
            return None;
        }
        let accent = token_rgb("--B60", "#005ae0");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let box_bg = if crate::color::is_dark() {
            hsla(0., 0., 1., 0.08)
        } else if msg.from_me {
            hsla(215., 0.8, 0.45, 0.10)
        } else {
            hsla(0., 0., 0., 0.045)
        };
        let sender = if msg.quote_sender.is_empty() {
            "Tin nhắn được trích dẫn".to_owned()
        } else {
            msg.quote_sender.clone()
        };
        let snippet = if msg.quote_text.is_empty() {
            "[Tin nhắn]".to_owned()
        } else {
            msg.quote_text.replace('\n', " ")
        };
        let jump_msg = msg.clone();
        Some(
            div()
                .id(("quote-card", msg.id))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .py(px(6.))
                .mb(px(6.))
                .rounded(px(4.))
                .bg(box_bg)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.jump_to_quote(&jump_msg);
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_none()
                        .w(px(2.5))
                        .h(px(30.))
                        .rounded(px(2.))
                        .bg(rgb(accent)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(ink))
                                .truncate()
                                .child(sender),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .text_color(rgb(muted))
                                .truncate()
                                .child(snippet),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn jump_to_quote(&mut self, msg: &ChatMsg) {
        let Some((new_window, child_idx)) = quote_scroll_index(
            self.messages_of(msg.conv),
            self.msg_window,
            msg.quote_global_id,
            msg.quote_cli_id,
        ) else {
            self.set_note("Không tìm thấy tin nhắn gốc trong lịch sử đã tải");
            return;
        };
        if new_window > self.msg_window {
            self.msg_window = new_window;
            self.warm_media(msg.conv);
        }
        if let Some(conv) = self.conv(msg.conv).cloned() {
            let (hidden, items) = self.build_msg_rows(&conv);
            self.sync_msg_list(conv.id, items.len(), hidden);
        }
        self.msg_list.scroll_to_reveal_item(child_idx);
    }

    pub(crate) fn msg_search_has_filter(&self) -> bool {
        !self.msg_search_q.trim().is_empty()
            || self.msg_search_sender.is_some()
            || self.msg_search_date_from.is_some()
            || self.msg_search_date_to.is_some()
    }

    /// Update in-chat search results from loaded messages + SQLite FTS/LIKE history,
    /// combining keyword, sender filter, and date-range filter ("From date".."To date").
    pub(crate) fn update_msg_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.msg_search_q = query.clone();
        self.msg_search_hits.clear();
        self.msg_search_idx = 0;
        let q = query.trim().to_lowercase();
        let Some(conv) = self.selected_conv() else {
            cx.notify();
            return;
        };
        if !self.msg_search_has_filter() {
            cx.notify();
            return;
        }
        // Merge any older matching DB records not yet held in `self.messages`
        // (supports `searchForEmpty: true` with date range, matching Electron `compact2438.js`).
        let from_ms = self.msg_search_date_from.map(|d| d.start_of_day_ms_local());
        let to_ms = self.msg_search_date_to.map(|d| d.end_of_day_ms_local());
        if !q.is_empty() || from_ms.is_some() || to_ms.is_some() {
            let extra = crate::data::search_thread_messages_filtered(
                &conv.thread_id,
                &q,
                from_ms,
                to_ms,
                200,
            );
            if !extra.is_empty() {
                let list = self.messages.entry(conv.id).or_default();
                let mut added = false;
                for item in extra {
                    let exists = list.iter().any(|m| {
                        m.id == item.id
                            || (item.cli_msg_id.is_some() && m.cli_msg_id == item.cli_msg_id)
                            || (item.global_msg_id.is_some()
                                && m.global_msg_id == item.global_msg_id)
                    });
                    if !exists {
                        list.push(item);
                        added = true;
                    }
                }
                if added {
                    list.sort_by_key(|m| m.at_ms);
                }
            }
        }
        let sender_filter = self.msg_search_sender.clone();
        let date_from = self.msg_search_date_from;
        let date_to = self.msg_search_date_to;
        self.msg_search_hits = self
            .messages_of(conv.id)
            .iter()
            .filter(|m| {
                if m.recalled {
                    return false;
                }
                if !q.is_empty()
                    && !m.text.to_lowercase().contains(&q)
                    && !m.file_name.to_lowercase().contains(&q)
                    && !m.sender.to_lowercase().contains(&q)
                {
                    return false;
                }
                if let Some(ref who) = sender_filter {
                    if who == "__me__" {
                        if !m.from_me {
                            return false;
                        }
                    } else if who == "__peer__" {
                        if m.from_me {
                            return false;
                        }
                    } else if m.from_me || (m.sender_uid != *who && m.sender != *who) {
                        return false;
                    }
                }
                if date_from.is_some() || date_to.is_some() {
                    if m.at_ms <= 0 {
                        return false;
                    }
                    let d = crate::calendar::CalDate::from_unix_ms_local(m.at_ms);
                    if let Some(from) = date_from {
                        if d < from {
                            return false;
                        }
                    }
                    if let Some(to) = date_to {
                        if d > to {
                            return false;
                        }
                    }
                }
                true
            })
            .map(|m| m.id)
            .collect();
        if !self.msg_search_hits.is_empty() {
            self.msg_search_idx = self.msg_search_hits.len() - 1;
            self.scroll_to_search_hit();
        }
        cx.notify();
    }

    pub(crate) fn step_msg_search(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.msg_search_hits.len();
        if len == 0 {
            return;
        }
        let next = (self.msg_search_idx as isize + delta).rem_euclid(len as isize) as usize;
        self.msg_search_idx = next;
        self.scroll_to_search_hit();
        cx.notify();
    }

    fn jump_to_search_hit_id(&mut self, msg_id: u64, cx: &mut Context<Self>) {
        if let Some(pos) = self.msg_search_hits.iter().position(|&id| id == msg_id) {
            self.msg_search_idx = pos;
            self.scroll_to_search_hit();
            cx.notify();
        }
    }

    fn scroll_to_search_hit(&mut self) {
        let Some(conv_id) = self.selected else {
            return;
        };
        let Some(&hit_id) = self.msg_search_hits.get(self.msg_search_idx) else {
            return;
        };
        let Some((new_window, child_idx)) =
            id_scroll_index(self.messages_of(conv_id), self.msg_window, hit_id)
        else {
            return;
        };
        if new_window > self.msg_window {
            self.msg_window = new_window;
            self.warm_media(conv_id);
        }
        if let Some(conv) = self.conv(conv_id).cloned() {
            let (hidden, items) = self.build_msg_rows(&conv);
            self.sync_msg_list(conv.id, items.len(), hidden);
        }
        self.msg_list.scroll_to_reveal_item(child_idx);
    }

    /// Right drawer "Search in Conversation" matching Zalo Electron/macOS:
    /// keyword input + "Filter by: Sender ▾ | Date ▾" with a full calendar
    /// range picker ("Month YYYY", Sun..Sat, "Suggested time", "Cancel"/"Confirm")
    /// and clickable message results list.
    pub(crate) fn msg_search_panel(
        &self,
        selected: Option<&ChatConv>,
        corners: CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let subtle = token_rgb("--layer-background-subtle", "#f3f5f8");
        let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
        let hover = token_rgb("--layer-background-hover", "#ebeff5");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let accent = token_rgb("--button-primary-normal", "#0068ff");
        let accent_hover = token_rgb("--button-primary-hover", "#005ae0");
        let chip_active_bg = if crate::color::is_dark() {
            0x17_31_57
        } else {
            0xe5_ef_ff
        };
        let chip_active_ink = if crate::color::is_dark() {
            0x66_ad_ff
        } else {
            accent
        };

        let has_q = !self.msg_search_q.trim().is_empty();
        let has_filter = self.msg_search_has_filter();
        let sender_active = self.msg_search_sender.is_some();
        let date_active =
            self.msg_search_date_from.is_some() || self.msg_search_date_to.is_some();

        let sender_chip_label = if sender_active && !self.msg_search_sender_label.is_empty() {
            self.msg_search_sender_label.clone()
        } else {
            "Người gửi".to_owned()
        };
        let date_chip_label = match (self.msg_search_date_from, self.msg_search_date_to) {
            (Some(f), Some(t)) if f == t => f.format_dmy(),
            (Some(f), Some(t)) => format!("{:02}/{:02} - {:02}/{:02}", f.day, f.month, t.day, t.month),
            (Some(f), None) => format!("Từ {}", f.format_dmy()),
            (None, Some(t)) => format!("Đến {}", t.format_dmy()),
            (None, None) => "Ngày gửi".to_owned(),
        };

        // Collect candidate senders `(filter_key, label, avatar_lookup_key)` for "Sender".
        let mut sender_opts: Vec<(Option<String>, String, Option<String>)> = vec![
            (None, "Tất cả".to_owned(), None),
            (
                Some("__me__".to_owned()),
                "Bạn".to_owned(),
                Some(crate::profiles::ME.to_owned()),
            ),
        ];
        if let Some(conv) = selected {
            if !conv.is_group {
                sender_opts.push((
                    Some("__peer__".to_owned()),
                    conv.name.clone(),
                    Some(conv.thread_id.clone()),
                ));
            } else {
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                for m in self.messages_of(conv.id) {
                    if m.from_me {
                        continue;
                    }
                    let key = if !m.sender_uid.is_empty() {
                        m.sender_uid.clone()
                    } else if !m.sender.is_empty() {
                        m.sender.clone()
                    } else {
                        continue;
                    };
                    if seen.insert(key.clone()) {
                        let label = if !m.sender.is_empty() {
                            m.sender.clone()
                        } else {
                            self.profiles
                                .map
                                .get(&key)
                                .map(|p| p.name.clone())
                                .filter(|n| !n.is_empty())
                                .unwrap_or_else(|| key.clone())
                        };
                        let av_key = (!m.sender_uid.is_empty()).then_some(m.sender_uid.clone());
                        sender_opts.push((Some(key), label, av_key));
                    }
                }
            }
        }

        // Build matching message rows (newest first, matching Electron).
        let hit_set: std::collections::HashSet<u64> =
            self.msg_search_hits.iter().copied().collect();
        let focused_id = self.msg_search_hits.get(self.msg_search_idx).copied();
        let result_rows: Vec<gpui::AnyElement> = selected
            .map(|conv| {
                let peer_fallback = conv.name.clone();
                self.messages_of(conv.id)
                    .iter()
                    .rev()
                    .filter(|m| hit_set.contains(&m.id))
                    .take(120)
                    .enumerate()
                    .map(|(idx, m)| {
                        let msg_id = m.id;
                        let is_focused = focused_id == Some(msg_id);
                        let who = if m.from_me {
                            "Bạn".to_owned()
                        } else if !m.sender.is_empty() {
                            m.sender.clone()
                        } else {
                            peer_fallback.clone()
                        };
                        let initial = who
                            .chars()
                            .next()
                            .map(|c| c.to_uppercase().to_string())
                            .unwrap_or_else(|| "Z".to_owned());
                        let when = if m.at_ms > 0 {
                            let d = crate::calendar::CalDate::from_unix_ms_local(m.at_ms);
                            if m.time.is_empty() {
                                d.format_dmy()
                            } else {
                                format!("{} {}", m.time, d.format_dmy())
                            }
                        } else {
                            m.time.clone()
                        };
                        let snippet = if !m.text.trim().is_empty() {
                            m.text.trim().replace('\n', " ")
                        } else if !m.file_name.is_empty() {
                            format!("[Tệp] {}", m.file_name)
                        } else {
                            "[Phương tiện]".to_owned()
                        };
                        let av_key = if m.from_me {
                            crate::profiles::ME
                        } else if !m.sender_uid.is_empty() {
                            m.sender_uid.as_str()
                        } else {
                            conv.thread_id.as_str()
                        };
                        let av_img = self.avatar_of(av_key);
                        let avatar_el = if let Some(img_av) = av_img {
                            gpui::img(img_av)
                                .flex_none()
                                .size(px(36.))
                                .rounded(px(18.))
                                .object_fit(gpui::ObjectFit::Cover)
                                .into_any_element()
                        } else {
                            div()
                                .flex_none()
                                .size(px(36.))
                                .rounded(px(18.))
                                .bg(rgb(if m.from_me { accent } else { subtle }))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(13.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(if m.from_me { 0xff_ff_ff } else { ink }))
                                .child(initial)
                                .into_any_element()
                        };
                        div()
                            .id(gpui::SharedString::from(format!("msearch-hit-{idx}")))
                            .w_full()
                            .px(px(12.))
                            .py(px(9.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(is_focused, |d| d.bg(rgb(chip_active_bg)))
                            .when(!is_focused, |d| d.hover(move |s| s.bg(rgb(hover))))
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(px(10.))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.msg_search_popup = None;
                                this.jump_to_search_hit_id(msg_id, cx);
                            }))
                            .child(avatar_el)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(6.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .truncate()
                                                    .text_size(px(13.))
                                                    .font_family(FAMILY_SEMIBOLD)
                                                    .text_color(rgb(ink))
                                                    .child(who),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .text_size(px(11.))
                                                    .text_color(rgb(muted))
                                                    .child(when),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(12.5))
                                            .text_color(rgb(muted))
                                            .child(snippet),
                                    ),
                            )
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Build calendar date-range popover matching the Zalo desktop UI.
        let date_popover = if self.msg_search_popup == Some("date") {
            let today = crate::calendar::CalDate::today();
            let (cal_y, cal_m) = self.msg_search_cal_ym;
            let grid = crate::calendar::month_grid(cal_y, cal_m);
            let draft_from = self.msg_search_draft_from;
            let draft_to = self.msg_search_draft_to;
            let from_text = draft_from
                .map(|d| d.format_dmy())
                .unwrap_or_else(|| "Từ ngày".to_owned());
            let to_text = draft_to
                .map(|d| d.format_dmy())
                .unwrap_or_else(|| "Đến ngày".to_owned());
            let can_confirm = draft_from.is_some() || draft_to.is_some();

            let mut cal_rows: Vec<gpui::AnyElement> = Vec::with_capacity(6);
            for row_idx in 0..6 {
                let mut row_cells: Vec<gpui::AnyElement> = Vec::with_capacity(7);
                for col_idx in 0..7 {
                    let cell = grid[row_idx * 7 + col_idx];
                    let is_today = cell.date == today;
                    let is_future = cell.date > today;
                    let is_edge =
                        draft_from == Some(cell.date) || draft_to == Some(cell.date);
                    let in_range = match (draft_from, draft_to) {
                        (Some(f), Some(t)) => cell.date > f && cell.date < t,
                        _ => false,
                    };
                    let clicked = cell.date;
                    let text_col = if is_edge {
                        rgb(0xff_ff_ff)
                    } else if !cell.in_month || is_future {
                        rgb(muted)
                    } else {
                        rgb(ink)
                    };
                    row_cells.push(
                        div()
                            .id(gpui::SharedString::from(format!(
                                "msearch-cal-{row_idx}-{col_idx}"
                            )))
                            .flex_1()
                            .h(px(30.))
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(is_edge, |d| d.bg(rgb(accent)))
                            .when(in_range && !is_edge, |d| d.bg(rgb(chip_active_bg)))
                            .when(!is_edge && is_today, |d| {
                                d.border_1().border_color(rgb(accent))
                            })
                            .when(!is_future, |d| {
                                d.cursor_pointer()
                                    .when(!is_edge, |d| d.hover(move |s| s.bg(rgb(hover))))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.msg_search_draft_slot == 0
                                            || this.msg_search_draft_from.is_none()
                                        {
                                            this.msg_search_draft_from = Some(clicked);
                                            if this
                                                .msg_search_draft_to
                                                .is_some_and(|t| t < clicked)
                                            {
                                                this.msg_search_draft_to = None;
                                            }
                                            this.msg_search_draft_slot = 1;
                                        } else {
                                            let f = this.msg_search_draft_from.unwrap_or(clicked);
                                            if clicked < f {
                                                this.msg_search_draft_from = Some(clicked);
                                                this.msg_search_draft_to = Some(f);
                                            } else {
                                                this.msg_search_draft_to = Some(clicked);
                                            }
                                            this.msg_search_draft_slot = 0;
                                        }
                                        this.msg_search_cal_ym = (clicked.year, clicked.month);
                                        cx.notify();
                                    }))
                            })
                            .when((!cell.in_month || is_future) && !is_edge, |d| {
                                d.opacity(0.38)
                            })
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .when(is_edge || is_today, |d| d.font_family(FAMILY_SEMIBOLD))
                                    .text_color(text_col)
                                    .child(format!("{}", cell.date.day)),
                            )
                            .into_any_element(),
                    );
                }
                cal_rows.push(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .gap(px(2.))
                        .children(row_cells)
                        .into_any_element(),
                );
            }

            let presets_open = self.msg_search_presets_open;
            let draft_slot = self.msg_search_draft_slot;
            // Matches Electron `compact3777.js`: presets apply immediately & close popover.
            let preset_btn = |id: &'static str, label: &'static str, days: i64| {
                div()
                    .id(id)
                    .px(px(9.))
                    .py(px(5.))
                    .rounded(px(5.))
                    .bg(rgb(subtle))
                    .hover(move |s| s.bg(rgb(hover)))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(rgb(ink))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let t = crate::calendar::CalDate::today();
                        let f = t.sub_days(days);
                        this.msg_search_draft_from = Some(f);
                        this.msg_search_draft_to = Some(t);
                        this.msg_search_date_from = Some(f);
                        this.msg_search_date_to = Some(t);
                        this.msg_search_cal_ym = (t.year, t.month);
                        this.msg_search_popup = None;
                        let q = this.msg_search_q.clone();
                        this.update_msg_search(q, cx);
                    }))
                    .child(label)
            };

            Some(
                div()
                    .id("msearch-date-popover")
                    .absolute()
                    .top(px(154.))
                    .right(px(12.))
                    .left(px(12.))
                    .rounded(px(8.))
                    .bg(rgb(paper))
                    .border_1()
                    .border_color(crate::color::border_subtle())
                    .shadow_lg()
                    .p(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .occlude()
                    // "Suggested time >"
                    .child(
                        div()
                            .id("msearch-presets-toggle")
                            .pb(px(6.))
                            .border_b_1()
                            .border_color(crate::color::border_subtle())
                            .cursor_pointer()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.msg_search_presets_open = !this.msg_search_presets_open;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(ink))
                                    .child("Gợi ý thời gian"),
                            )
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(rgb(muted))
                                    .child(if presets_open { "⌄" } else { "›" }),
                            ),
                    )
                    .when(presets_open, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .gap(px(6.))
                                .child(preset_btn("ms-pre-7", "7 ngày trước", 7))
                                .child(preset_btn("ms-pre-30", "1 tháng trước", 30))
                                .child(preset_btn("ms-pre-90", "3 tháng trước", 90)),
                        )
                    })
                    // "Select date range" + clickable From date / To date slots
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(ink))
                                    .child("Chọn khoảng thời gian"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .id("msearch-slot-from")
                                            .flex_1()
                                            .h(px(32.))
                                            .px(px(9.))
                                            .rounded(px(6.))
                                            .border_1()
                                            .cursor_pointer()
                                            .when(draft_slot == 0, |d| {
                                                d.border_color(rgb(accent))
                                            })
                                            .when(draft_slot != 0, |d| {
                                                d.border_color(crate::color::border_subtle())
                                            })
                                            .bg(rgb(paper))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .justify_between()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.msg_search_draft_slot = 0;
                                                if let Some(f) = this.msg_search_draft_from {
                                                    this.msg_search_cal_ym = (f.year, f.month);
                                                }
                                                cx.notify();
                                            }))
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(if draft_from.is_some() {
                                                        rgb(ink)
                                                    } else {
                                                        rgb(muted)
                                                    })
                                                    .child(from_text),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(muted))
                                                    .child("📅"),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("msearch-slot-to")
                                            .flex_1()
                                            .h(px(32.))
                                            .px(px(9.))
                                            .rounded(px(6.))
                                            .border_1()
                                            .cursor_pointer()
                                            .when(draft_slot == 1, |d| {
                                                d.border_color(rgb(accent))
                                            })
                                            .when(draft_slot != 1, |d| {
                                                d.border_color(crate::color::border_subtle())
                                            })
                                            .bg(rgb(paper))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .justify_between()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.msg_search_draft_slot = 1;
                                                if let Some(t) = this.msg_search_draft_to {
                                                    this.msg_search_cal_ym = (t.year, t.month);
                                                }
                                                cx.notify();
                                            }))
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(if draft_to.is_some() {
                                                        rgb(ink)
                                                    } else {
                                                        rgb(muted)
                                                    })
                                                    .child(to_text),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(muted))
                                                    .child("📅"),
                                            ),
                                    ),
                            ),
                    )
                    // Month header: `Month YYYY`  <  >
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(ink))
                                    .child(format!("Tháng {cal_m}, {cal_y}")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .id("msearch-cal-prev")
                                            .size(px(24.))
                                            .rounded(px(5.))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(rgb(hover)))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(px(15.))
                                            .text_color(rgb(ink))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                let (y, m) = this.msg_search_cal_ym;
                                                this.msg_search_cal_ym =
                                                    crate::calendar::prev_month(y, m);
                                                cx.notify();
                                            }))
                                            .child("‹"),
                                    )
                                    .child(
                                        div()
                                            .id("msearch-cal-next")
                                            .size(px(24.))
                                            .rounded(px(5.))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(rgb(hover)))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(px(15.))
                                            .text_color(rgb(ink))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                let (y, m) = this.msg_search_cal_ym;
                                                this.msg_search_cal_ym =
                                                    crate::calendar::next_month(y, m);
                                                cx.notify();
                                            }))
                                            .child("›"),
                                    ),
                            ),
                    )
                    // Weekday row: CN T2 T3 T4 T5 T6 T7
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .gap(px(2.))
                            .children(["CN", "T2", "T3", "T4", "T5", "T6", "T7"].into_iter().map(
                                |w| {
                                    div()
                                        .flex_1()
                                        .h(px(22.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(11.5))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(ink))
                                        .child(w)
                                },
                            )),
                    )
                    // 6×7 calendar cells
                    .child(div().flex().flex_col().gap(px(2.)).children(cal_rows))
                    // Footer: Clear filter | Cancel | Confirm
                    .child(
                        div()
                            .pt(px(4.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div().when(date_active || can_confirm, |d| {
                                    d.child(
                                        div()
                                            .id("msearch-date-reset")
                                            .px(px(8.))
                                            .py(px(6.))
                                            .rounded(px(5.))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(rgb(hover)))
                                            .text_size(px(12.))
                                            .text_color(rgb(muted))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.msg_search_draft_from = None;
                                                this.msg_search_draft_to = None;
                                                this.msg_search_date_from = None;
                                                this.msg_search_date_to = None;
                                                this.msg_search_popup = None;
                                                let q = this.msg_search_q.clone();
                                                this.update_msg_search(q, cx);
                                            }))
                                            .child("Xóa lọc"),
                                    )
                                }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .id("msearch-date-cancel")
                                            .h(px(32.))
                                            .px(px(14.))
                                            .rounded(px(5.))
                                            .bg(rgb(subtle))
                                            .hover(move |s| s.bg(rgb(hover)))
                                            .cursor_pointer()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(px(13.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(ink))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.msg_search_popup = None;
                                                cx.notify();
                                            }))
                                            .child("Hủy"),
                                    )
                                    .child(
                                        div()
                                            .id("msearch-date-confirm")
                                            .h(px(32.))
                                            .px(px(14.))
                                            .rounded(px(5.))
                                            .bg(rgb(accent))
                                            .when(!can_confirm, |d| d.opacity(0.45))
                                            .when(can_confirm, |d| {
                                                d.cursor_pointer()
                                                    .hover(move |s| s.bg(rgb(accent_hover)))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        cx.stop_propagation();
                                                        let f = this.msg_search_draft_from;
                                                        let t = this.msg_search_draft_to.or(f);
                                                        this.msg_search_date_from = f;
                                                        this.msg_search_date_to = t;
                                                        this.msg_search_popup = None;
                                                        let q = this.msg_search_q.clone();
                                                        this.update_msg_search(q, cx);
                                                    }))
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(px(13.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(0xff_ff_ff))
                                            .child("Xác nhận"),
                                    ),
                            ),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };

        // Sender dropdown popover with real circular user avatars
        let sender_popover = if self.msg_search_popup == Some("sender") {
            let items: Vec<gpui::AnyElement> = sender_opts
                .into_iter()
                .enumerate()
                .map(|(idx, (key, label, av_key))| {
                    let on = self.msg_search_sender == key;
                    let key_c = key.clone();
                    let label_c = label.clone();
                    let av_img = av_key.as_deref().and_then(|k| self.avatar_of(k));
                    let initial = label
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().to_string())
                        .unwrap_or_else(|| "Z".to_owned());
                    let av_el = if let Some(img_av) = av_img {
                        gpui::img(img_av)
                            .flex_none()
                            .size(px(22.))
                            .rounded(px(11.))
                            .object_fit(gpui::ObjectFit::Cover)
                            .into_any_element()
                    } else if key.is_none() {
                        div()
                            .flex_none()
                            .size(px(22.))
                            .rounded(px(11.))
                            .bg(rgb(subtle))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.))
                            .text_color(rgb(muted))
                            .child("●")
                            .into_any_element()
                    } else {
                        div()
                            .flex_none()
                            .size(px(22.))
                            .rounded(px(11.))
                            .bg(rgb(if key.as_deref() == Some("__me__") {
                                accent
                            } else {
                                subtle
                            }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(10.5))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(if key.as_deref() == Some("__me__") {
                                0xff_ff_ff
                            } else {
                                ink
                            }))
                            .child(initial)
                            .into_any_element()
                    };
                    div()
                        .id(gpui::SharedString::from(format!("msearch-sender-{idx}")))
                        .h(px(36.))
                        .px(px(10.))
                        .cursor_pointer()
                        .when(on, |d| d.bg(rgb(chip_active_bg)))
                        .hover(move |s| s.bg(rgb(hover)))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.msg_search_sender = key_c.clone();
                            this.msg_search_sender_label = if key_c.is_some() {
                                label_c.clone()
                            } else {
                                String::new()
                            };
                            this.msg_search_popup = None;
                            let q = this.msg_search_q.clone();
                            this.update_msg_search(q, cx);
                        }))
                        .child(av_el)
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(13.))
                                .when(on, |d| d.font_family(FAMILY_SEMIBOLD))
                                .text_color(rgb(ink))
                                .child(label),
                        )
                        .when(on, |d| {
                            d.child(
                                div()
                                    .text_size(px(12.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(chip_active_ink))
                                    .child("✓"),
                            )
                        })
                        .into_any_element()
                })
                .collect();
            Some(
                div()
                    .id("msearch-sender-popover")
                    .absolute()
                    .top(px(154.))
                    .left(px(58.))
                    .w(px(220.))
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .rounded(px(8.))
                    .bg(rgb(paper))
                    .border_1()
                    .border_color(crate::color::border_subtle())
                    .shadow_lg()
                    .py(px(4.))
                    .flex()
                    .flex_col()
                    .occlude()
                    .children(items)
                    .into_any_element(),
            )
        } else {
            None
        };

        let has_popup = self.msg_search_popup.is_some();
        corners
            .apply(
                div()
                    .id("chat-search-drawer")
                    .relative()
                    .flex_none()
                    .w(px(crate::info::INFO_PANEL_W))
                    .h_full()
                    .bg(rgb(paper))
                    .border_l_1()
                    .border_color(crate::color::border_subtle())
                    .flex()
                    .flex_col(),
                false,
                false,
                false,
                true,
            )
            // Header ("Search in Conversation" + `×`, 68 px matching info_panel)
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h(px(zalo_design::CHAT_HEADER_H))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(crate::color::border_subtle())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child("Tìm kiếm trong trò chuyện"),
                    )
                    .child(
                        div()
                            .id("hdr-search-close")
                            .absolute()
                            .right(px(12.))
                            .size(px(28.))
                            .rounded(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(hover)))
                            .text_size(px(18.))
                            .text_color(rgb(muted))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.msg_search_open = false;
                                this.msg_search_popup = None;
                                this.msg_search_hits.clear();
                                this.msg_search_q.clear();
                                this.msg_search_sender = None;
                                this.msg_search_sender_label.clear();
                                this.msg_search_date_from = None;
                                this.msg_search_date_to = None;
                                this.msg_search.update(cx, |i, cx| i.clear(cx));
                                cx.notify();
                            }))
                            .child("×"),
                    ),
            )
            // Search input box + "Filter by: Sender ▾ | Date ▾"
            .child(
                div()
                    .flex_none()
                    .px(px(14.))
                    .pt(px(14.))
                    .pb(px(10.))
                    .border_b_1()
                    .border_color(crate::color::border_subtle())
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .w_full()
                            .h(px(34.))
                            .px(px(10.))
                            .gap(px(6.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .bg(rgb(field))
                            .child(div().text_color(rgb(muted)).child(fonts::icon_el(
                                ic::HDR_SEARCH,
                                "⌕",
                                15.,
                            )))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(13.))
                                    .text_color(rgb(ink))
                                    .child(self.msg_search.clone()),
                            )
                            .when(has_q, |d| {
                                d.child(
                                    div()
                                        .id("msearch-clear-q")
                                        .size(px(20.))
                                        .rounded(px(10.))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(rgb(hover)))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(13.))
                                        .text_color(rgb(muted))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.msg_search.update(cx, |i, cx| i.clear(cx));
                                            this.update_msg_search(String::new(), cx);
                                        }))
                                        .child("×"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(12.5))
                                    .text_color(rgb(muted))
                                    .child("Lọc theo:"),
                            )
                            .child(
                                div()
                                    .id("msearch-filter-sender")
                                    .flex_1()
                                    .min_w(px(0.))
                                    .h(px(28.))
                                    .px(px(8.))
                                    .rounded(px(5.))
                                    .bg(rgb(if sender_active {
                                        chip_active_bg
                                    } else {
                                        subtle
                                    }))
                                    .hover(move |s| s.bg(rgb(hover)))
                                    .cursor_pointer()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(4.))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.msg_search_popup =
                                            if this.msg_search_popup == Some("sender") {
                                                None
                                            } else {
                                                Some("sender")
                                            };
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(px(12.))
                                            .when(sender_active, |d| d.font_family(FAMILY_SEMIBOLD))
                                            .text_color(rgb(if sender_active {
                                                chip_active_ink
                                            } else {
                                                ink
                                            }))
                                            .child(sender_chip_label),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(if sender_active {
                                                chip_active_ink
                                            } else {
                                                muted
                                            }))
                                            .child("▾"),
                                    ),
                            )
                            .child(
                                div()
                                    .id("msearch-filter-date")
                                    .flex_1()
                                    .min_w(px(0.))
                                    .h(px(28.))
                                    .px(px(8.))
                                    .rounded(px(5.))
                                    .bg(rgb(if date_active {
                                        chip_active_bg
                                    } else {
                                        subtle
                                    }))
                                    .hover(move |s| s.bg(rgb(hover)))
                                    .cursor_pointer()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(4.))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        if this.msg_search_popup == Some("date") {
                                            this.msg_search_popup = None;
                                        } else {
                                            this.msg_search_draft_from = this.msg_search_date_from;
                                            this.msg_search_draft_to = this.msg_search_date_to;
                                            this.msg_search_draft_slot = 0;
                                            let focus = this
                                                .msg_search_date_from
                                                .unwrap_or_else(crate::calendar::CalDate::today);
                                            this.msg_search_cal_ym = (focus.year, focus.month);
                                            this.msg_search_popup = Some("date");
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(px(12.))
                                            .when(date_active, |d| d.font_family(FAMILY_SEMIBOLD))
                                            .text_color(rgb(if date_active {
                                                chip_active_ink
                                            } else {
                                                ink
                                            }))
                                            .child(date_chip_label),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(if date_active {
                                                chip_active_ink
                                            } else {
                                                muted
                                            }))
                                            .child("▾"),
                                    ),
                            ),
                    ),
            )
            // Scrollable search results body
            .child(
                div()
                    .id("msearch-results-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .when(!has_filter, |d| {
                        d.items_center().justify_center().pt(px(64.)).child(
                            div()
                                .max_w(px(240.))
                                .text_size(px(13.))
                                .text_color(rgb(muted))
                                .child("Nhập từ khóa hoặc lọc theo Người gửi / Ngày gửi để tìm tin nhắn trong cuộc trò chuyện."),
                        )
                    })
                    .when(has_filter && result_rows.is_empty(), |d| {
                        d.items_center().justify_center().pt(px(64.)).child(
                            div()
                                .text_size(px(13.5))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(muted))
                                .child("Không tìm thấy tin nhắn phù hợp"),
                        )
                    })
                    .when(has_filter && !result_rows.is_empty(), |d| {
                        let count = self.msg_search_hits.len();
                        d.child(
                            div()
                                .px(px(6.))
                                .pb(px(4.))
                                .text_size(px(12.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(muted))
                                .child(format!("Tin nhắn ({count})")),
                        )
                        .children(result_rows)
                    }),
            )
            .when(has_popup, |d| {
                d.child(
                    div()
                        .id("msearch-popup-dismiss")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.msg_search_popup = None;
                            cx.notify();
                        })),
                )
            })
            .when_some(sender_popover, |d, pop| d.child(pop))
            .when_some(date_popover, |d, pop| d.child(pop))
    }

    /// One Electron `.card--group-photo`: cells in a row, caption once underneath.
    fn album_body(
        &self,
        album: &[ChatMsg],
        ink: u32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        const MAX_W: f32 = 400.0;
        const GAP: f32 = 2.0;
        let cols = if album.len() <= 4 { album.len() } else { 3 };
        let cols = cols.max(1);
        let cell_w = ((MAX_W - GAP * (cols as f32 - 1.0)) / cols as f32).max(72.0);
        let cell_h = if cols >= 4 { 110.0 } else { 140.0 };
        let field = token_rgb("--layer-background-subtle", "#ebecf0");
        let mut col = div().flex().flex_col().gap(px(GAP)).flex_none();
        for chunk in album.chunks(cols) {
            let mut row = div().flex().flex_row().gap(px(GAP)).flex_none();
            for msg in chunk {
                row = row.child(self.album_cell(msg, cell_w, cell_h, field, cx));
            }
            col = col.child(row);
        }
        let caption = album_caption(album);
        if !caption.is_empty() {
            col = col.child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .max_w(px(MAX_W))
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .text_color(rgb(ink))
                    .child(caption),
            );
        }
        col.into_any_element()
    }

    fn album_cell(
        &self,
        msg: &ChatMsg,
        w: f32,
        h: f32,
        field: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = msg.clone();
        let image = self.media_bitmaps.get(&msg.id).cloned();
        div()
            .id(("album-cell", msg.id))
            .w(px(w))
            .h(px(h))
            .flex_none()
            .overflow_hidden()
            .rounded(px(4.))
            .bg(rgb(field))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_viewer(open.clone(), window, cx);
                cx.notify();
            }))
            .when_some(image, |el, image| {
                el.child(
                    img(image)
                        .w(px(w))
                        .h(px(h))
                        .object_fit(ObjectFit::Cover),
                )
            })
    }

    fn bubble_body(
        &self,
        msg: &ChatMsg,
        fill: u32,
        ink: u32,
        muted: u32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let blue = token_rgb("--B60", "#005ae0");
        let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
        if msg.recalled {
            return wrapped_text(str_vi("STR_UNDO_MESSAGE", "Tin nhắn đã thu hồi"), muted);
        }
        match msg.kind {
            MsgKind::Photo | MsgKind::Gif | MsgKind::Doodle => {
                let (w, h) = media::photo_size(msg.media_w, msg.media_h);
                let mut col = div().flex().flex_col().gap(px(6.)).w(px(w));
                col = col.child(self.photo_block(msg, w, h, field, cx));
                if !media::hide_marker_text(msg.kind, &msg.text) {
                    col = col.child(wrapped_text(&msg.text, ink));
                }
                col.into_any_element()
            }
            MsgKind::Sticker => {
                let size = media::STICKER_SIZE;
                self.photo_block(msg, size, size, fill, cx)
                    .into_any_element()
            }
            MsgKind::File => {
                let name = msg.file_name.clone();
                let local = msg
                    .media_path
                    .clone()
                    .filter(|p| p.is_file())
                    .or_else(|| media::find_downloaded(&msg.file_name));
                let size = if msg.file_size > 0 {
                    msg.file_size
                } else {
                    local
                        .as_ref()
                        .and_then(|p| std::fs::metadata(p).ok())
                        .map(|m| m.len())
                        .unwrap_or(0)
                };
                let show_thumb = self.prefs.bool("fileThumb", true);
                let ext = media::text_file_ext(&msg.file_name);
                let preview = if show_thumb {
                    ext.as_ref()
                        .zip(local.as_ref())
                        .and_then(|(_, path)| media::read_text_preview(path))
                } else {
                    None
                };
                let on_disk = local.is_some();
                let card_w = zalo_design::FILE_CARD_W - media::BUBBLE_PAD * 2.0;
                let open_name = name.clone();
                let open_path = local.clone();
                let view_name = name.clone();
                let view_path = local.clone();
                let folder_name = name.clone();
                let folder_path = local.clone();
                let save_name = name.clone();
                let save_path = local.clone();
                let msg_id = msg.id;
                let icon_ink = token_rgb("--text-primary", "#081b3a");
                let actions = div()
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .when(ext.is_some(), |el| {
                        let tip_text = str_vi("STR_TFE_VIEW_TIP", "Xem File không cần tải");
                        el.child(file_icon_btn(
                            ("file-view", msg_id),
                            '\u{EB2B}',
                            "👁",
                            tip_text,
                            icon_ink,
                            cx.listener(move |this, event: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                if event.standard_click() {
                                    this.open_file(&view_name, view_path.as_deref(), cx);
                                    cx.notify();
                                }
                            }),
                        ))
                    })
                    .when(on_disk, |el| {
                        let tip_text = str_vi("STR_VIEW_IN_FOLDER", "Hiển thị trong thư mục");
                        el.child(file_icon_btn(
                            ("file-folder", msg_id),
                            '\u{EC50}',
                            "📁",
                            tip_text,
                            icon_ink,
                            cx.listener(move |this, event: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                if event.standard_click() {
                                    this.reveal_file(&folder_name, folder_path.as_deref(), cx);
                                    cx.notify();
                                }
                            }),
                        ))
                    })
                    .child({
                        let tip_text = str_vi("STR_DOWNLOAD_FILE", "Lưu về máy");
                        file_icon_btn(
                            ("file-save", msg_id),
                            '\u{EB29}',
                            "⬇",
                            tip_text,
                            icon_ink,
                            cx.listener(move |this, event: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                if event.standard_click() {
                                    this.save_file_download(&save_name, save_path.as_deref(), cx);
                                    cx.notify();
                                }
                            }),
                        )
                    });
                let card = if show_thumb {
                    let mut col = div().flex().flex_col().w(px(card_w));
                    if let (Some(lines), Some(kind)) = (preview.as_ref(), ext.as_ref()) {
                        col = col.child(text_preview_box(lines, kind, card_w));
                    }
                    col.child(file_card(
                        &msg.file_name,
                        size,
                        on_disk,
                        ink,
                        muted,
                        actions,
                    ))
                    .into_any_element()
                } else {
                    let display = if msg.file_name.is_empty() {
                        "Tệp đính kèm".to_owned()
                    } else {
                        msg.file_name.clone()
                    };
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .child(fonts::icon_el(ic::TB_ATTACH, "📎", 16.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .max_w(px(240.))
                                        .truncate()
                                        .text_size(px(14.))
                                        .line_height(px(20.))
                                        .text_color(rgb(ink))
                                        .child(display),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .line_height(px(16.))
                                        .text_color(rgb(muted))
                                        .child(if size > 0 {
                                            media::format_file_size(size)
                                        } else {
                                            str_vi("STR_FILE", "File").to_owned()
                                        }),
                                ),
                        )
                        .child(actions)
                        .into_any_element()
                };
                div()
                    .id(("file-card", msg.id))
                    .cursor_pointer()
                    .tooltip(|_, cx| tip(str_vi("STR_OPEN", "Mở"), cx))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.standard_click() {
                            this.open_file(&open_name, open_path.as_deref(), cx);
                            cx.notify();
                        }
                    }))
                    .child(card)
                    .into_any_element()
            }
            MsgKind::Link => {
                let host = if msg.file_name.is_empty() {
                    media::host_of(&msg.href).to_owned()
                } else {
                    msg.file_name.clone()
                };
                let href = msg.href.clone();
                // Zalo PC link message: the URL in blue, then a card with
                // thumbnail, bold title, two-line description and site.
                let preview = self
                    .link_previews
                    .get(&msg.href)
                    .cloned()
                    .filter(|p| !p.is_empty());
                let thumb = preview
                    .as_ref()
                    .and_then(|p| self.link_thumbs.get(&p.thumb).cloned());
                div()
                    .id(("link-card", msg.id))
                    .cursor_pointer()
                    .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                        if event.standard_click() && href.starts_with("http") {
                            cx.open_url(&href);
                        }
                    }))
                    .map(|el| match preview {
                        None => el.child(link_card(&host, &msg.href, blue, muted, field)),
                        Some(p) => {
                            let url_trim = msg.href.trim();
                            let title_is_url = p.title.trim().is_empty()
                                || p.title.trim() == url_trim
                                || p.title.trim().starts_with("http://")
                                || p.title.trim().starts_with("https://");
                            let desc_is_dup = p.desc.trim().is_empty()
                                || p.desc.trim() == p.title.trim()
                                || p.desc.trim() == url_trim
                                || p.desc.trim().starts_with("http://")
                                || p.desc.trim().starts_with("https://");
                            let generic_thumb = title_is_url
                                || p.thumb.contains("default")
                                || p.thumb.contains("placeholder");
                            let show_thumb = if generic_thumb { None } else { thumb };
                            if title_is_url && desc_is_dup && show_thumb.is_none() {
                                return el.child(link_card(&host, &msg.href, blue, muted, field));
                            }
                            let display_title = if title_is_url {
                                host.clone()
                            } else {
                                clip_chars(&p.title, 90)
                            };
                            el.flex()
                                .flex_col()
                                .w(px(300.))
                                .gap(px(6.))
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .line_height(px(20.))
                                        .text_color(rgb(blue))
                                        .child(media::wrap_bubble_text(&msg.href, 290.)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .rounded(px(6.))
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(crate::color::border_subtle())
                                        .bg(rgb(crate::color::token_rgb(
                                            "--layer-background",
                                            "#ffffff",
                                        )))
                                        .when_some(show_thumb, |el, t| {
                                            el.child(
                                                img(t)
                                                    .w(px(300.))
                                                    .h(px(156.))
                                                    .object_fit(gpui::ObjectFit::Cover),
                                            )
                                        })
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap(px(4.))
                                                .p(px(10.))
                                                .child(
                                                    div()
                                                        .text_size(px(14.))
                                                        .line_height(px(20.))
                                                        .font_family(fonts::FAMILY_SEMIBOLD)
                                                        .text_color(rgb(ink))
                                                        .child(media::wrap_bubble_text(
                                                            &display_title,
                                                            270.,
                                                        )),
                                                )
                                                .when(!desc_is_dup, |el| {
                                                    el.child(
                                                        div()
                                                            .text_size(px(13.))
                                                            .line_height(px(18.))
                                                            .text_color(rgb(muted))
                                                            .child(media::wrap_bubble_text(
                                                                &clip_chars(&p.desc, 110),
                                                                270.,
                                                            )),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .text_size(px(13.))
                                                        .text_color(rgb(blue))
                                                        .child(if p.src.is_empty() {
                                                            host.clone()
                                                        } else {
                                                            p.src.clone()
                                                        }),
                                                ),
                                        ),
                                )
                        }
                    })
                    .into_any_element()
            }
            MsgKind::Call | MsgKind::MissedCall => {
                // Zalo PC `.call-msg`: bold title, icon + kind, divider,
                // "Call back" in blue.
                let missed = msg.kind == MsgKind::MissedCall;
                let conv_of_call = msg.conv;
                let red = token_rgb("--text-errors", "#c31818");
                let group = msg.file_name == "Cuộc gọi nhóm";
                let title = if group {
                    "Cuộc gọi nhóm".to_owned()
                } else if missed {
                    if msg.from_me {
                        "Người nhận bị nhỡ"
                    } else {
                        "Bạn bị nhỡ"
                    }
                    .to_owned()
                } else if msg.from_me {
                    "Cuộc gọi thoại đi".to_owned()
                } else {
                    "Cuộc gọi thoại đến".to_owned()
                };
                let (icon, icon_color, sub) = if missed {
                    (fonts::ic::CALL_MISS, red, "Cuộc gọi thoại")
                } else if msg.from_me {
                    (
                        fonts::ic::CALL_OUT,
                        token_rgb("--G60", "#15a85f"),
                        "Cuộc gọi thoại",
                    )
                } else {
                    (
                        fonts::ic::CALL_IN,
                        token_rgb("--G60", "#15a85f"),
                        "Cuộc gọi thoại",
                    )
                };
                div()
                    .flex()
                    .flex_col()
                    .w(px(176.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_family(fonts::FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child(crate::i18n::t(&title)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .mt(px(6.))
                            .text_size(px(14.))
                            .text_color(rgb(muted))
                            .child(
                                div()
                                    .text_color(rgb(icon_color))
                                    .child(fonts::icon_el(icon, "", 18.)),
                            )
                            .child(sub),
                    )
                    .child(
                        div()
                            .h(px(1.))
                            .mt(px(10.))
                            .bg(rgb(token_rgb("--divider", "#000000"))),
                    )
                    .child(
                        div()
                            .id(("call-back", msg.id))
                            .h(px(36.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_size(px(15.))
                            .font_family(fonts::FAMILY_SEMIBOLD)
                            .text_color(rgb(blue))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.start_call(conv_of_call, false, cx);
                                cx.notify();
                            }))
                            .child("Gọi lại"),
                    )
                    .into_any_element()
            }
            MsgKind::Voice => {
                // Zalo PC voice bubble: round play button, bars, length.
                let playing = self
                    .voice_playing
                    .as_ref()
                    .is_some_and(|(id, _)| *id == msg.id);
                let play = msg.clone();
                let bars = [
                    6., 12., 18., 10., 22., 14., 8., 16., 20., 12., 6., 14., 18., 10., 8., 12.,
                ];
                div()
                    .id(("voice", msg.id))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .w(px(220.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_voice(&play, cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_none()
                            .size(px(32.))
                            .rounded(px(16.))
                            .bg(rgb(token_rgb("--B55", "#0068ff")))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(rgb(0xffffff))
                            .text_size(px(13.))
                            .child(if playing { "■" } else { "▶" }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(2.))
                            .flex_1()
                            .children(bars.into_iter().map(move |h| {
                                div().w(px(3.)).h(px(h)).rounded(px(1.5)).bg(if playing {
                                    rgb(blue)
                                } else {
                                    rgb(muted)
                                })
                            })),
                    )
                    .child(div().text_size(px(13.)).text_color(rgb(muted)).child(
                        if msg.file_size > 0 {
                            crate::links::clock(msg.file_size)
                        } else {
                            "Thoại".to_owned()
                        },
                    ))
                    .into_any_element()
            }
            MsgKind::Video => {
                let open = msg.clone();
                let (w, h) = media::photo_size(msg.media_w.max(1), msg.media_h.max(1));
                let (w, h) = if msg.media_w == 0 {
                    (240., 160.)
                } else {
                    (w, h)
                };
                let thumb = self.media_bitmaps.get(&msg.id).cloned();
                div()
                    .id(("video", msg.id))
                    .relative()
                    .w(px(w))
                    .h(px(h))
                    .rounded(px(zalo_design::CARD_RADIUS))
                    .overflow_hidden()
                    .bg(rgb(0x1f_23_29))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.open_video(&open, cx);
                        cx.notify();
                    }))
                    .when_some(thumb, |el, t| {
                        el.child(img(t).size_full().object_fit(gpui::ObjectFit::Cover))
                    })
                    .child(
                        div()
                            .absolute()
                            .inset(px(0.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .size(px(44.))
                                    .rounded(px(22.))
                                    .bg(hsla(0., 0., 0., 0.55))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(rgb(0xffffff))
                                    .text_size(px(18.))
                                    .child("▶"),
                            ),
                    )
                    .when(msg.file_size > 0, |el| {
                        el.child(
                            div()
                                .absolute()
                                .right(px(6.))
                                .bottom(px(6.))
                                .px(px(6.))
                                .rounded(px(4.))
                                .bg(hsla(0., 0., 0., 0.55))
                                .text_size(px(12.))
                                .text_color(rgb(0xffffff))
                                .child(crate::links::clock(msg.file_size)),
                        )
                    })
                    .into_any_element()
            }
            MsgKind::Card => {
                let title = if msg.file_name.is_empty() {
                    str_vi("STR_RECOMMENDED_USER", "Danh thiếp").to_owned()
                } else {
                    msg.file_name.clone()
                };
                let subtitle = card_blurb(&msg.text);
                let photo = self.media_bitmaps.get(&msg.id).cloned();
                let initial = title.chars().next().unwrap_or('?').to_string();
                let face = match photo {
                    Some(image) => img(image)
                        .size(px(40.))
                        .object_fit(ObjectFit::Cover)
                        .into_any_element(),
                    None => div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(0xffffff))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_size(px(16.))
                        .child(initial)
                        .into_any_element(),
                };
                div()
                    .flex()
                    .flex_col()
                    .w(px(220.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .size(px(40.))
                                    .flex_none()
                                    .rounded(px(20.))
                                    .overflow_hidden()
                                    .bg(rgb(token_rgb("--B55", "#0068ff")))
                                    .child(face),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w(px(0.))
                                    .child(
                                        div()
                                            .text_size(px(15.))
                                            .line_height(px(22.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(ink))
                                            .truncate()
                                            .child(title),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(muted))
                                            .truncate()
                                            .child(subtitle),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
            MsgKind::Poll => self.poll_bubble(msg, ink, muted, field, blue, cx),
            MsgKind::Location => place_card(msg, "Vị trí", "Mở bản đồ", ink, muted, blue, cx),
            MsgKind::Ecard => place_card(msg, "Thiệp", "Mở liên kết", ink, muted, blue, cx),
            MsgKind::Text => {
                if let Some(note) = media::phone_note(&msg.text) {
                    self.phone_link_card(msg, &note, ink, muted, cx)
                } else if let Some(url) =
                    media::first_link(&msg.text).filter(|url| msg.text.trim() != url)
                {
                    self.text_with_link(msg, &url, ink, muted, blue, field, cx)
                } else {
                    bubble_text(&msg.text, ink)
                }
            }
        }
    }

    /// Sentence that contains a URL: the text, then a small link card.
    fn text_with_link(
        &self,
        msg: &ChatMsg,
        url: &str,
        ink: u32,
        muted: u32,
        blue: u32,
        field: u32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let preview = self
            .link_previews
            .get(url)
            .cloned()
            .filter(|p| !p.is_empty());
        let thumb = preview
            .as_ref()
            .and_then(|p| self.link_thumbs.get(&p.thumb).cloned());
        let host = media::host_of(url).to_owned();
        let href = url.to_owned();
        let title = preview
            .as_ref()
            .map(|p| p.title.clone())
            .unwrap_or_default();
        let desc = preview.as_ref().map(|p| p.desc.clone()).unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .max_w(px(300.))
            .child(bubble_text(&msg.text, ink))
            .child(
                div()
                    .id(("inline-link", msg.id))
                    .cursor_pointer()
                    .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if event.standard_click() && href.starts_with("http") {
                            cx.open_url(&href);
                        }
                    }))
                    .child(if preview.is_some() {
                        div()
                            .flex()
                            .flex_col()
                            .w(px(280.))
                            .rounded(px(6.))
                            .overflow_hidden()
                            .border_1()
                            .border_color(rgb(token_rgb("--border-subtle", "#000000")))
                            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
                            .when_some(thumb, |el, image| {
                                el.child(
                                    img(image)
                                        .w(px(280.))
                                        .h(px(120.))
                                        .object_fit(ObjectFit::Cover),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .p(px(8.))
                                    .when(!title.is_empty(), |el| {
                                        el.child(
                                            div()
                                                .text_size(px(13.))
                                                .font_family(FAMILY_SEMIBOLD)
                                                .text_color(rgb(ink))
                                                .truncate()
                                                .child(title),
                                        )
                                    })
                                    .when(!desc.is_empty(), |el| {
                                        el.child(
                                            div()
                                                .text_size(px(12.))
                                                .text_color(rgb(muted))
                                                .truncate()
                                                .child(desc),
                                        )
                                    })
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(blue))
                                            .truncate()
                                            .child(host),
                                    ),
                            )
                            .into_any_element()
                    } else {
                        link_card(&host, url, blue, muted, field)
                    }),
            )
            .into_any_element()
    }

    /// Linked phone: sender as the title, the number as a blue copy link,
    /// caption underneath. The clock stays on the canvas, outside the card.
    fn phone_link_card(
        &self,
        msg: &ChatMsg,
        note: &media::PhoneNote,
        ink: u32,
        muted: u32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let blue = token_rgb("--link", "#0068ff");
        let from_profile = self
            .profiles
            .map
            .get(&msg.sender_uid)
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty());
        let title = from_profile.unwrap_or_else(|| msg.sender.clone());
        let title = if title.trim().is_empty() {
            "Số điện thoại".to_owned()
        } else {
            title
        };
        let number = note.phone.clone();
        let shown = number.clone();
        let tip_text = "Sao chép, gọi Zalo hoặc kết bạn".to_owned();
        let caption = note.caption.clone();
        div()
            .flex()
            .flex_col()
            .w(px(220.))
            .gap(px(2.))
            .child(
                div()
                    .text_size(px(15.))
                    .line_height(px(22.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child(title),
            )
            .child(
                div()
                    .id(("phone-link", msg.id))
                    .cursor_pointer()
                    .text_size(px(15.))
                    .line_height(px(22.))
                    .whitespace_nowrap()
                    .text_color(rgb(blue))
                    .tooltip(move |_, cx| tip(tip_text.clone(), cx))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.standard_click() {
                            this.open_menu(
                                MenuKind::Phone {
                                    phone: number.clone(),
                                },
                                event.position(),
                                window,
                            );
                            cx.notify();
                        }
                    }))
                    .child(shown),
            )
            .when(!caption.is_empty(), |el| {
                el.child(
                    div()
                        .mt(px(2.))
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .text_color(rgb(muted))
                        .child(caption),
                )
            })
            .into_any_element()
    }

    /// `group.poll` card: question, one vote per row, closed subtitle.
    fn poll_bubble(
        &self,
        msg: &ChatMsg,
        ink: u32,
        muted: u32,
        field: u32,
        blue: u32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let question = if msg.file_name.is_empty() {
            msg.text
                .trim()
                .strip_prefix("[Bình chọn]")
                .unwrap_or(msg.text.trim())
                .trim()
                .to_owned()
        } else {
            msg.file_name.clone()
        };
        let question = if question.is_empty() {
            "Bình chọn".to_owned()
        } else {
            question
        };
        let poll_id = msg.poll_id;
        let closed = msg.poll_closed;
        let options = msg.poll_options.clone();
        let total: i64 = options.iter().map(|opt| opt.votes.max(0)).sum();
        let mut col = div()
            .flex()
            .flex_col()
            .w(px(308.))
            .max_w(px(308.))
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(15.))
                    .line_height(px(22.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child(media::wrap_bubble_text(&question, 292.)),
            );
        if closed {
            col = col.child(
                div()
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(rgb(muted))
                    .child("Đã kết thúc"),
            );
        }
        if options.is_empty() {
            col = col.child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(muted))
                    .child("Đang tải lựa chọn…"),
            );
        }
        for (index, opt) in options.iter().enumerate() {
            let oid = opt.id;
            let voted = opt.voted;
            let can_vote = !closed && oid > 0 && poll_id > 0;
            let frac = if total > 0 {
                opt.votes.max(0) as f32 / total as f32
            } else {
                0.0
            };
            let bar = (frac * 276.0).clamp(0.0, 276.0);
            col = col.child(
                div()
                    .id(SharedString::from(format!("poll-opt-{}-{index}", msg.id)))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if voted {
                        gpui::Hsla::from(rgb(blue))
                    } else {
                        crate::color::border_subtle()
                    })
                    .bg(rgb(field))
                    .when(can_vote, |el| {
                        el.cursor_pointer().on_click(cx.listener(
                            move |this, event: &ClickEvent, _, cx| {
                                if event.standard_click() {
                                    this.vote_on_poll(poll_id, oid, cx);
                                    cx.notify();
                                }
                            },
                        ))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(13.))
                                    .line_height(px(18.))
                                    .text_color(rgb(ink))
                                    .child(opt.content.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(muted))
                                    .child(format!("{} phiếu", opt.votes.max(0))),
                            ),
                    )
                    .child(
                        div()
                            .h(px(3.))
                            .w_full()
                            .rounded(px(2.))
                            .bg(rgb(token_rgb("--divider-subtle", "#000000")))
                            .child(div().h(px(3.)).w(px(bar)).rounded(px(2.)).bg(rgb(if voted {
                                blue
                            } else {
                                muted
                            }))),
                    ),
            );
        }
        col.into_any_element()
    }

    fn photo_block(
        &self,
        msg: &ChatMsg,
        w: f32,
        h: f32,
        field: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let placeholder = div()
            .w(px(w))
            .h(px(h))
            .rounded(px(zalo_design::CARD_RADIUS))
            .bg(rgb(field))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(12.))
            .text_color(rgb(token_rgb("--text-secondary", "#5a6981")))
            .child(if self.media_failed.contains(&msg.id) {
                str_vi("STR_CLOUD_PHOTO_EXPIRED", "Ảnh đã hết hạn")
            } else if self.media_pending.contains(&msg.id) {
                str_vi("STR_LOADING", "Đang tải...")
            } else if msg.media_url.is_empty() && msg.media_path.is_none() {
                // Old rows saved only the image placeholder marker, no attachment.
                "Không có dữ liệu ảnh"
            } else {
                match msg.kind {
                    MsgKind::Sticker => "Sticker",
                    MsgKind::Gif => "GIF",
                    MsgKind::Doodle => "Vẽ hình",
                    _ => str_vi("STR_PHOTO", "Hình ảnh"),
                }
            });
        if let Some(image) = self.media_bitmaps.get(&msg.id).cloned() {
            let open = msg.clone();
            div()
                .id(("photo", msg.id))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_viewer(open.clone(), window, cx);
                    cx.notify();
                }))
                .child(
                    img(image)
                        .w(px(w))
                        .h(px(h))
                        .rounded(px(zalo_design::CARD_RADIUS))
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element()
        } else {
            placeholder.into_any_element()
        }
    }
}

/// Location or e-card: eyebrow, title, optional subtitle, and a link when `href` is http(s).
fn place_card(
    msg: &ChatMsg,
    eyebrow: &str,
    link_label: &str,
    ink: u32,
    muted: u32,
    blue: u32,
    cx: &mut Context<ShellView>,
) -> gpui::AnyElement {
    let marker = format!("[{eyebrow}]");
    let rest = msg
        .text
        .trim()
        .strip_prefix(marker.as_str())
        .unwrap_or(msg.text.trim())
        .trim()
        .to_owned();
    let title = if msg.file_name.is_empty() {
        if rest.is_empty() {
            eyebrow.to_owned()
        } else {
            rest.clone()
        }
    } else {
        msg.file_name.clone()
    };
    let subtitle = if rest.is_empty() || rest == title {
        String::new()
    } else {
        rest
    };
    let href = msg.href.clone();
    let open = href.starts_with("http://") || href.starts_with("https://");
    div()
        .id(("place", msg.id))
        .w(px(240.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .when(open, |el| {
            el.cursor_pointer()
                .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                    if event.standard_click() {
                        cx.open_url(&href);
                    }
                }))
        })
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgb(blue))
                .child(eyebrow.to_owned()),
        )
        .child(
            div()
                .text_size(px(15.))
                .line_height(px(22.))
                .font_family(FAMILY_SEMIBOLD)
                .text_color(rgb(ink))
                .child(title),
        )
        .when(!subtitle.is_empty(), |el| {
            el.child(
                div()
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .text_color(rgb(muted))
                    .child(subtitle),
            )
        })
        .when(open, |el| {
            el.child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(blue))
                    .child(link_label.to_owned()),
            )
        })
        .into_any_element()
}

fn bubble_font() -> Font {
    Font {
        family: FAMILY_TEXT.into(),
        features: FontFeatures::default(),
        fallbacks: Some(FontFallbacks::from_fonts(vec![
            "Noto Color Emoji".into(),
            "Noto Emoji".into(),
            "Segoe UI Emoji".into(),
        ])),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

fn bubble_text(text: &str, color: u32) -> gpui::AnyElement {
    use crate::emoji::PictogramPart;
    let parts = crate::emoji::split_pictograms(text);
    if parts
        .iter()
        .all(|part| matches!(part, PictogramPart::Text(_)))
    {
        return wrapped_text(text, color);
    }
    pictogram_parts(parts, color)
}

fn pictogram_parts(parts: Vec<crate::emoji::PictogramPart>, color: u32) -> gpui::AnyElement {
    use crate::emoji::PictogramPart;
    div()
        .max_w(px(media::BUBBLE_INNER))
        .min_w(px(0.))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_end()
        .gap(px(2.))
        .text_size(px(15.))
        .line_height(px(22.))
        .text_color(rgb(color))
        .font(bubble_font())
        .children(parts.into_iter().enumerate().map(|(i, part)| match part {
            PictogramPart::Text(run) => div()
                .id(("emo-run", i as u64))
                .whitespace_normal()
                .child(media::wrap_bubble_text(&run, media::BUBBLE_INNER))
                .into_any_element(),
            PictogramPart::Tile(item) => div()
                .id(("emo-tile", i as u64))
                .child(
                    img(item.path.clone())
                        .size(px(20.))
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
        }))
        .into_any_element()
}

fn wrapped_text(text: &str, color: u32) -> gpui::AnyElement {
    // `.text-message__container span-15`: 15 px / 22 px; a bare link is
    // `a.text-is-link` `#0068ff`.
    let wrapped = media::wrap_bubble_text(text, media::BUBBLE_INNER);
    let width = media::text_box_w(&wrapped);
    let is_link = media::first_link(text).is_some_and(|u| text.trim() == u);
    div()
        .w(px(width))
        .max_w(px(media::BUBBLE_INNER))
        .min_w(px(0.))
        .whitespace_normal()
        .text_size(px(15.))
        .line_height(px(22.))
        .text_color(rgb(if is_link {
            token_rgb("--B55", "#0068ff")
        } else {
            color
        }))
        .font(bubble_font())
        .child(wrapped)
        .into_any_element()
}

/// Electron `.fl-th-txt`: 374×195, radius 4, above the file row.
fn text_preview_box(lines: &[String], ext: &str, card_w: f32) -> gpui::AnyElement {
    let preview_w = card_w + 22.0;
    let inset = (preview_w - card_w) / 2.0;
    let layer = token_rgb("--layer-background", "#ffffff");
    let gutter_bg = token_rgb("--surface-background-subtle", "#ebecf0");
    let border = token_rgb("--border", "#d6dbe1");
    let divider = token_rgb("--divider", "#e6e8ec");
    let gutter_w = 36.0;
    let text_w = preview_w - gutter_w - 16.0;
    let mut box_el = div()
        .w(px(preview_w))
        .h(px(195.))
        .ml(px(-inset))
        .mr(px(-inset))
        .mb(px(15.))
        .rounded(px(4.))
        .border_1()
        .border_color(rgb(border))
        .overflow_hidden()
        .bg(rgb(layer))
        .flex()
        .flex_col();
    for (i, line) in lines.iter().enumerate() {
        box_el = box_el.child(preview_line(i + 1, line, ext, gutter_w, text_w));
    }
    box_el
        .child(
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h(px(0.))
                .child(
                    div()
                        .w(px(gutter_w))
                        .h_full()
                        .bg(rgb(gutter_bg))
                        .border_r_1()
                        .border_color(rgb(divider)),
                )
                .child(div().flex_1().h_full().bg(rgb(layer))),
        )
        .into_any_element()
}

fn preview_line(
    index: usize,
    line: &str,
    ext: &str,
    gutter_w: f32,
    text_w: f32,
) -> gpui::AnyElement {
    let gutter_bg = token_rgb("--surface-background-subtle", "#ebecf0");
    let divider = token_rgb("--divider", "#e6e8ec");
    let num_ink = token_rgb("--text-secondary", "#5a6981");
    let layer = token_rgb("--layer-background", "#ffffff");
    let runs = media::preview_runs(line, ext);
    div()
        .flex()
        .flex_row()
        .w_full()
        .child(
            div()
                .flex()
                .flex_none()
                .justify_end()
                .items_start()
                .w(px(gutter_w))
                .pt(px(1.))
                .pr(px(6.))
                .bg(rgb(gutter_bg))
                .border_r_1()
                .border_color(rgb(divider))
                .text_size(px(12.))
                .line_height(px(18.))
                .font_family("DejaVu Sans Mono")
                .text_color(rgb(num_ink))
                .child(index.to_string()),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .flex_1()
                .min_w(px(0.))
                .px(px(8.))
                .bg(rgb(layer))
                .children(runs.into_iter().map(|(text, tone)| {
                    let color = preview_tone_rgb(tone);
                    div()
                        .max_w(px(text_w))
                        .min_w(px(0.))
                        .whitespace_normal()
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .font_family("DejaVu Sans Mono")
                        .when(tone == media::PreviewTone::Header, |el| {
                            el.font_weight(FontWeight::BOLD)
                        })
                        .text_color(rgb(color))
                        .child(text)
                })),
        )
        .into_any_element()
}

/// CodeMirror colours Electron uses on the file thumb.
fn preview_tone_rgb(tone: media::PreviewTone) -> u32 {
    let dark = crate::color::is_dark();
    match tone {
        media::PreviewTone::Body => token_rgb("--text-primary", "#081b3a"),
        media::PreviewTone::Header => {
            if dark {
                0x6897bb
            } else {
                0x0000ff
            }
        }
        media::PreviewTone::Code => {
            if dark {
                0xcc7832
            } else {
                0xaa5500
            }
        }
    }
}

/// `.file-message-v2`: 38×48 badge, size, "Saved on device", action icons.
fn file_card(
    name: &str,
    size: u64,
    on_disk: bool,
    ink: u32,
    muted: u32,
    actions: impl IntoElement,
) -> gpui::AnyElement {
    let ext = media::file_ext_label(name);
    let badge = media::file_ext_color(name);
    let meta = if size > 0 {
        media::format_file_size(size)
    } else {
        str_vi("STR_FILE", "File").to_owned()
    };
    let title = if name.is_empty() {
        str_vi("STR_FILE", "File").to_owned()
    } else {
        name.to_owned()
    };
    let ok = token_rgb("--G70", "#2b974f");
    let inner = zalo_design::FILE_CARD_W - media::BUBBLE_PAD * 2.0;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .w(px(inner))
        .max_w(px(inner))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .w(px(38.))
                .h(px(48.))
                .rounded(px(4.))
                .bg(rgb(badge))
                .text_color(rgb(0xffffff))
                .text_size(px(11.))
                .font_family(FAMILY_SEMIBOLD)
                .child(ext),
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
                        .line_height(px(20.))
                        .text_color(rgb(ink))
                        .truncate()
                        .child(crate::i18n::t(&title)),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .min_h(px(24.))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.))
                                .min_w(px(0.))
                                .flex_1()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(13.))
                                        .line_height(px(18.))
                                        .text_color(rgb(muted))
                                        .child(meta),
                                )
                                .when(on_disk, |el| {
                                    el.child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap(px(4.))
                                            .min_w(px(0.))
                                            .text_color(rgb(ok))
                                            .child(fonts::icon_el('\u{EC54}', "✓", 14.))
                                            .child(
                                                div()
                                                    .min_w(px(0.))
                                                    .text_size(px(13.))
                                                    .line_height(px(18.))
                                                    .text_color(rgb(ok))
                                                    .truncate()
                                                    .child(str_vi(
                                                        "STR_FILE_DOWNLOAD_SUCCESS_STATUS",
                                                        "Đã có trên máy",
                                                    )),
                                            ),
                                    )
                                }),
                        )
                        .child(actions),
                ),
        )
        .into_any_element()
}

/// Square action on a file card (`fa-file-view`, folder, download).
fn file_icon_btn(
    id: impl Into<gpui::ElementId>,
    icon: char,
    fallback: &'static str,
    tip_text: &'static str,
    icon_ink: u32,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::AnyElement {
    let border = token_rgb("--border", "#d6dbe1");
    let fill = token_rgb("--layer-background", "#ffffff");
    let hover = token_rgb("--surface-background-subtle", "#ebecf0");
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded(px(6.))
        .bg(rgb(fill))
        .border_1()
        .border_color(rgb(border))
        .text_color(rgb(icon_ink))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(hover)))
        .tooltip(move |_, cx| tip(tip_text, cx))
        .on_click(on_click)
        .child(fonts::icon_el(icon, fallback, 14.))
        .into_any_element()
}

/// Extra lines under a contact card message, or the "Contact Card" label.
fn card_blurb(text: &str) -> String {
    let rest = text
        .trim()
        .strip_prefix("[Danh thiếp]")
        .unwrap_or(text.trim());
    let extra: String = rest
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if extra.is_empty() {
        str_vi("STR_RECOMMENDED_USER", "Danh thiếp").to_owned()
    } else {
        extra
    }
}

/// First `max` characters plus «…» (cards keep two short lines).
fn clip_chars(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

fn link_card(host: &str, href: &str, blue: u32, muted: u32, field: u32) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .w(px(300.))
        .px(px(10.))
        .py(px(8.))
        .bg(rgb(field))
        .rounded(px(zalo_design::CARD_RADIUS))
        .child(
            div()
                .text_size(px(13.))
                .line_height(px(18.))
                .text_color(rgb(blue))
                .truncate()
                .child(host.to_owned()),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(rgb(muted))
                .truncate()
                .child(href.to_owned()),
        )
        .into_any_element()
}

/// Solid cover over a bubble while "Privacy Cover" is on. Hover reveals it.
fn peek_cover(group: &SharedString, fill: u32) -> gpui::AnyElement {
    div()
        .absolute()
        .inset_0()
        .rounded(px(zalo_design::CARD_RADIUS))
        .bg(rgb(fill))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .text_color(rgb(token_rgb("--text-secondary", "#5a6981")))
        .group_hover(group.clone(), |st| st.invisible())
        .child("Đã ẩn")
        .into_any_element()
}

fn album_caption(album: &[ChatMsg]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for msg in album {
        let text = msg.text.trim();
        if text.is_empty() || media::hide_marker_text(MsgKind::Photo, text) {
            continue;
        }
        if !lines.iter().any(|line| line == text) {
            lines.push(text.to_owned());
        }
    }
    lines.join("\n")
}

fn preview_action(
    id: (&'static str, u64),
    glyph: char,
    fallback: &'static str,
    label: &'static str,
    ink: u32,
    click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .cursor_pointer()
        .text_color(rgb(ink))
        .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
        .tooltip(move |_, cx| tip(label, cx))
        .on_click(click)
        .child(fonts::icon_el(glyph, fallback, 14.))
}

/// Zalo's yellow like at `size` px, or a text fallback.
fn like_el(size: f32) -> gpui::AnyElement {
    match media::like_icon() {
        Some(image) => img(image).size(px(size)).into_any_element(),
        None => div()
            .text_size(px(size * 0.8))
            .child("👍")
            .into_any_element(),
    }
}

/// Same sender, same day, ≤ 10 min apart → one visual block.
fn same_block(a: &ChatMsg, b: &ChatMsg) -> bool {
    a.from_me == b.from_me
        && a.sender_uid == b.sender_uid
        && (a.at_ms == 0 || b.at_ms == 0 || (b.at_ms - a.at_ms).abs() <= 10 * 60_000)
        && (a.at_ms == 0 || b.at_ms == 0 || data::vn_day(a.at_ms) == data::vn_day(b.at_ms))
}

/// Names under "Seen (n)" and "Received (n)", the same split as Zalo PC's popup.
fn receipt_panel(pill: &OutgoingStatus) -> gpui::AnyElement {
    let ink = token_rgb("--text-primary", "#081b3a");
    let muted = token_rgb("--text-secondary", "#5a6981");
    let mut col = div()
        .mt(px(4.))
        .p(px(8.))
        .w(px(220.))
        .rounded(px(6.))
        .bg(rgb(token_rgb("--layer-background", "#ffffff")))
        .shadow_sm()
        .flex()
        .flex_col()
        .gap(px(2.));
    if !pill.seen.is_empty() {
        let title = if pill.all_seen {
            format!("Đã xem ({}) · Tất cả", pill.seen.len())
        } else {
            format!("Đã xem ({})", pill.seen.len())
        };
        col = col.child(receipt_heading(&title, muted));
        for (_, name) in &pill.seen {
            col = col.child(receipt_name(name, ink));
        }
    }
    if !pill.delivered.is_empty() {
        col = col.child(receipt_heading(
            &format!("Đã nhận ({})", pill.delivered.len()),
            muted,
        ));
        for (_, name) in &pill.delivered {
            col = col.child(receipt_name(name, ink));
        }
    }
    col.into_any_element()
}

fn receipt_heading(text: &str, color: u32) -> gpui::AnyElement {
    div()
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(rgb(color))
        .child(text.to_owned())
        .into_any_element()
}

fn receipt_name(text: &str, color: u32) -> gpui::AnyElement {
    div()
        .text_size(px(13.))
        .line_height(px(18.))
        .text_color(rgb(color))
        .truncate()
        .child(text.to_owned())
        .into_any_element()
}

/// 40 px peer avatar beside the first bubble of a block.
fn small_avatar(
    name: &str,
    uid: &str,
    photo: Option<std::sync::Arc<gpui::RenderImage>>,
) -> gpui::AnyElement {
    if let Some(p) = photo {
        return img(p)
            .size(px(40.))
            .rounded(px(20.))
            .object_fit(ObjectFit::Cover)
            .into_any_element();
    }
    crate::sidebar::letter_disc(
        &crate::sidebar::avatar_initials(name),
        crate::sidebar::avatar_color(uid),
        40.,
    )
    .into_any_element()
}

/// Theme-aware `--timestamp` pill background (Electron: `rgba(0,0,0,0.2)` light,
/// translucent white in dark mode so date/time/receipt chips stay legible).
fn timestamp_pill_bg() -> gpui::Hsla {
    if crate::color::is_dark() {
        hsla(0., 0., 1., 0.12)
    } else {
        hsla(0., 0., 0., 0.2)
    }
}

fn date_chip(label: &str) -> impl IntoElement {
    let pill_bg = timestamp_pill_bg();
    div()
        .flex()
        .justify_center()
        .w_full()
        .pt(px(16.))
        .pb(px(4.))
        .child(
            div()
                .px(px(10.))
                .pt(px(2.))
                .pb(px(3.))
                .line_height(px(20.))
                .rounded(px(zalo_design::CHAT_DATE_RADIUS))
                .bg(pill_bg)
                .text_color(rgb(0xffffff))
                .text_size(px(13.))
                .child(crate::i18n::t(&label)),
        )
}

fn placeholder(title: &'static str, body: &'static str, corners: CsdCorners) -> impl IntoElement {
    let paper = token_rgb("--layer-background", "#ffffff");
    let ink = token_rgb("--text-primary", "#081b3a");
    let muted = token_rgb("--text-secondary", "#5a6981");
    let line = crate::color::border_subtle();
    corners
        .apply(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .h_full()
                .min_w(px(0.))
                .bg(rgb(paper)),
            false,
            false,
            false,
            true,
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .flex_none()
                .h(px(zalo_design::CHAT_HEADER_H))
                .px(px(16.))
                .border_b_1()
                .border_color(line)
                .text_size(px(16.))
                .font_family(FAMILY_SEMIBOLD)
                .text_color(rgb(ink))
                .child(crate::i18n::t(&title)),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .px(px(32.))
                .text_size(px(14.))
                .text_color(rgb(muted))
                .child(body),
        )
}

impl ShellView {
    #[allow(clippy::too_many_arguments)]
    fn header_identity(
        &self,
        conv: Option<&ChatConv>,
        members: Option<usize>,
        typing_line: Option<String>,
        photo: Option<std::sync::Arc<gpui::RenderImage>>,
        is_stranger: bool,
        label: Option<String>,
        online: bool,
        activity: Option<(String, bool)>,
        cloud: bool,
        ink: u32,
        muted: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let conv_id = conv.map(|row| row.id);
        let has_prev = members.is_some() || activity.is_some();
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
    let blue = token_rgb("--B55", "#0068ff");
    let (name, initial, fill) = match conv {
        Some(row) => {
            let display = if cloud {
                "My Documents".to_owned()
            } else {
                row.name.clone()
            };
            let init = crate::sidebar::avatar_initials(&display);
            (
                display,
                init,
                if row.is_group {
                    0x0068ff
                } else {
                    crate::sidebar::avatar_color(&row.thread_id)
                },
            )
        }
        None => ("Zalo".to_owned(), "Z".to_owned(), 0x0068ff),
    };
    let is_group = conv.is_some_and(|row| row.is_group);
    let faces = if is_group && photo.is_none() && !cloud {
        conv.map(|row| self.group_collage_faces(&row.thread_id))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let paint = crate::sidebar::avatar_paint(is_group, photo.is_some(), cloud, faces.len());
    div()
        .flex()
        .flex_row()
        .items_center()
        .min_w(px(0.))
        .child(
            div()
                .relative()
                .size(px(48.))
                .flex_none()
                .when(paint == crate::sidebar::AvatarPaint::Photo, |el| {
                    el.when_some(photo.clone(), |el, p| {
                        el.child(
                            img(p)
                                .size(px(48.))
                                .rounded(px(24.))
                                .object_fit(ObjectFit::Cover),
                        )
                    })
                })
                .when(paint == crate::sidebar::AvatarPaint::Cloud, |el| {
                    el.child(
                        div()
                            .size(px(48.))
                            .rounded(px(24.))
                            .bg(rgb(blue))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(rgb(0xffffff))
                            .child(fonts::icon_el(ic::DOCS_ON, "☁", 32.)),
                    )
                })
                .when(paint == crate::sidebar::AvatarPaint::Collage, |el| {
                    el.child(crate::sidebar::group_collage(&faces, 48.))
                })
                .when(paint == crate::sidebar::AvatarPaint::GroupMark, |el| {
                    el.child(crate::sidebar::group_mark(48.))
                })
                .when(paint == crate::sidebar::AvatarPaint::Letter, |el| {
                    el.child(crate::sidebar::letter_disc(&initial, fill, 48.))
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
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .ml(px(12.))
                .min_w(px(0.))
                .child(
                    div()
                        .h(px(28.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .min_w(px(0.))
                        .child(
                            div()
                                .min_w(px(0.))
                                .text_size(px(18.))
                                .line_height(px(27.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(ink))
                                .truncate()
                                .child(name),
                        )
                        .when(is_stranger, |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .px(px(6.))
                                    .py(px(1.))
                                    .rounded(px(3.))
                                    .bg(rgb(token_rgb("--layer-background-subtle", "#eef0f3")))
                                    .text_color(rgb(muted))
                                    .text_size(px(10.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .child("NGƯỜI LẠ"),
                            )
                        }),
                )
                .child(match typing_line {
                    Some(line) => div()
                        .h(px(21.))
                        .text_size(px(14.))
                        .line_height(px(21.))
                        .text_color(rgb(blue))
                        .truncate()
                        .child(line),
                    None => div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(px(21.))
                        .text_size(px(14.))
                        .line_height(px(21.))
                        .text_color(rgb(ink))
                        .when_some(members, |el, n| {
                            el.child(fonts::icon_el(ic::HDR_MEMBERS, "☺", 18.))
                                .child(div().ml(px(4.)).child(crate::i18n::members(n)))
                        })
                        .when_some(activity, |el, (text, fresh)| {
                            let color = if fresh {
                                token_rgb("--G60", "#15a85f")
                            } else {
                                muted
                            };
                            el.child(
                                div()
                                    .text_color(rgb(color))
                                    .truncate()
                                    .child(crate::i18n::t(&text)),
                            )
                        })
                        .when_some(conv_id, |el, id| {
                            el.when(has_prev, |row| {
                                row.child(
                                    div()
                                        .mx(px(8.))
                                        .w(px(1.))
                                        .h(px(12.))
                                        .bg(crate::color::border_subtle()),
                                )
                            })
                            .child(
                                div()
                                    .id("hdr-tag-btn")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .h(px(21.))
                                    .px(px(4.))
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .text_color(rgb(muted))
                                    .hover(move |st| st.bg(rgb(hover)))
                                    .tooltip(|_, cx| tip("Phân loại", cx))
                                    .on_click(cx.listener(
                                        move |this, e: &ClickEvent, window, cx| {
                                            this.open_menu(
                                                MenuKind::Conv(id),
                                                e.position(),
                                                window,
                                            );
                                            cx.notify();
                                        },
                                    ))
                                    .child(fonts::icon_el(ic::HDR_TAG, "⌂", 16.))
                                    .when_some(label, |btn, text| {
                                        btn.child(
                                            div()
                                                .ml(px(4.))
                                                .text_color(rgb(muted))
                                                .truncate()
                                                .child(text),
                                        )
                                    }),
                            )
                        }),
                }),
        )
    }
}

/// Computes `(msg_window, child_index)` to scroll `#message-view` to the target quoted message.
pub(crate) fn quote_scroll_index(
    all: &[ChatMsg],
    cur_window: usize,
    target_gid: Option<i64>,
    target_cid: Option<i64>,
) -> Option<(usize, usize)> {
    msg_scroll_index(all, cur_window, |m| {
        (target_gid.is_some() && m.global_msg_id == target_gid)
            || (target_cid.is_some() && m.cli_msg_id == target_cid)
    })
}

/// Computes `(msg_window, child_index)` to scroll `#message-view` to a message matching `msg_id`.
pub(crate) fn id_scroll_index(
    all: &[ChatMsg],
    cur_window: usize,
    msg_id: u64,
) -> Option<(usize, usize)> {
    msg_scroll_index(all, cur_window, |m| m.id == msg_id)
}

fn msg_scroll_index(
    all: &[ChatMsg],
    cur_window: usize,
    pred: impl Fn(&ChatMsg) -> bool,
) -> Option<(usize, usize)> {
    let idx = all.iter().position(pred)?;
    let from_end = all.len() - idx;
    let step = crate::shell::MSG_WINDOW.max(1);
    let new_window = if from_end > cur_window {
        from_end.div_ceil(step) * step
    } else {
        cur_window
    };
    let hidden = all.len().saturating_sub(new_window);
    let msgs = &all[hidden..];
    let target_slice_idx = idx - hidden;
    let mut item_idx = usize::from(hidden > 0);
    let mut last_day: Option<i64> = None;
    for (i, msg) in msgs.iter().enumerate() {
        let day = (msg.at_ms > 0).then(|| data::vn_day(msg.at_ms));
        if day != last_day {
            item_idx += 1;
            last_day = day;
        }
        if i == target_slice_idx {
            return Some((new_window, item_idx));
        }
        item_idx += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_chrome_sizes() {
        assert_eq!(zalo_design::CHAT_HEADER_H, 68.0);
        assert_eq!(COMPOSER_BAR_H, 41.0);
        assert_eq!(zalo_design::COMPOSER_TOOL_BTN, 40.0);
        assert_eq!(zalo_design::COMPOSER_TOOL_ICON, 18.0);
        assert_eq!(zalo_design::COMPOSER_TOOL_RADIUS, 8.0);
        assert_eq!(zalo_design::HEADER_ACTION_ICON, 20.0);
        assert_eq!(zalo_design::HEADER_ACTION_BTN, 32.0);
        assert_eq!(zalo_design::CARD_RADIUS, 8.0);
        assert_eq!(zalo_design::FILE_CARD_W, 376.0);
        assert_eq!(zalo_design::CHAT_DATE_RADIUS, 10.0);
        assert_eq!(COMPOSER_INPUT_H, 51.0);
        assert_eq!(media::BUBBLE_MAX, 360.0);
        assert_eq!(media::STICKER_SIZE, 120.0);
        assert_eq!(token_rgb("--layer-background-subtle", "#000"), 0xebec_f0);
        assert_eq!(token_rgb("--blue-message", "#000"), 0xe5f1_ff);
        assert_eq!(token_rgb("--white-message", "#000"), 0xff_ffff);
    }

    #[test]
    fn quote_scroll_index_expands_window_and_counts_day_chips() {
        let base_ts = 1_794_400_000_000_i64;
        let msgs: Vec<ChatMsg> = (0..70_u64)
            .map(|i| {
                let ts = base_ts + if i >= 5 { 86_400_000 } else { 0 };
                ChatMsg::text_msg(
                    i + 1,
                    10,
                    "5".into(),
                    i % 2 == 0,
                    "Lan".into(),
                    "5".into(),
                    format!("msg {i}"),
                    "10:00".into(),
                    ts,
                    Some((i + 100) as i64),
                    Some((i + 1000) as i64),
                )
            })
            .collect();
        // Old message at index 2 (gid 1002): beyond MSG_WINDOW=60, so window expands to 120.
        let (win, child_idx) =
            quote_scroll_index(&msgs, crate::shell::MSG_WINDOW, Some(1002), None).unwrap();
        assert_eq!(win, crate::shell::MSG_WINDOW * 2);
        // hidden == 0 (since len 70 <= 120), index 0 inserts day chip 0, index 1 & 2 are same day -> child index 3.
        assert_eq!(child_idx, 3);
        // New message in current window (gid 1069): stays in window 60 (`hidden == 10`), has `"load-older"` + 1 day chip.
        let (win2, child_idx2) =
            quote_scroll_index(&msgs, crate::shell::MSG_WINDOW, Some(1069), None).unwrap();
        assert_eq!(win2, crate::shell::MSG_WINDOW);
        assert_eq!(child_idx2, 1 + 1 + 59);
        assert_eq!(
            id_scroll_index(&msgs, crate::shell::MSG_WINDOW, 3),
            Some((win, 3))
        );
        assert_eq!(id_scroll_index(&msgs, crate::shell::MSG_WINDOW, 9999), None);
    }

    #[test]
    fn unicode_formatting_and_mention_query_behave_cleanly() {
        let bold = format_unicode_style("Zalo 2026!", TextStyleKind::Bold);
        assert_eq!(bold, "𝐙𝐚𝐥𝐨 𝟐𝟎𝟐𝟔!");
        assert_eq!(
            format_unicode_style(&bold, TextStyleKind::Bold),
            "Zalo 2026!"
        );
        assert_eq!(
            format_unicode_style("hello", TextStyleKind::Italic),
            "ℎ𝑒𝑙𝑙𝑜"
        );
        assert_eq!(format_unicode_style("hi", TextStyleKind::Mono), "𝚑𝚒");
        let struck = format_unicode_style("Abc", TextStyleKind::Strike);
        assert_eq!(struck, "A\u{0336}b\u{0336}c\u{0336}");
        assert_eq!(format_unicode_style(&struck, TextStyleKind::Strike), "Abc");

        assert_eq!(mention_query("@"), Some((0, "")));
        assert_eq!(mention_query("Chào @minh"), Some(("Chào ".len(), "minh")));
        assert_eq!(mention_query("email@domain.com"), None);
        assert_eq!(mention_query("@gif:mèo"), None);
        assert_eq!(mention_query("@sticker"), None);
        assert_eq!(mention_query("@an đã gửi"), None);
    }
}
