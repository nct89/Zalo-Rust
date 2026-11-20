//! `.context-menu` from compact-app CSS.
//!
//! Local pin/mute/hide/reply/copy/delete always run. Live P1/P6 calls fire
//! only when the thread is not a preview fixture and a session exists.

use gpui::{
    div, prelude::*, px, rgb, Context, Focusable, InteractiveElement, IntoElement, ParentElement,
    Pixels, Point, StatefulInteractiveElement, Styled,
};

use crate::bridge::{self, LiveAction};
use crate::color::{str_vi, token_rgb};
use crate::data::{self, ChatMsg};
use crate::shell::{ListFilter, ShellView};

/// Open menu: kind + window-space click.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenMenu {
    /// Which surface opened the menu.
    pub kind: MenuKind,
    /// X in chrome pixels (already clamped inside the window).
    pub x: f32,
    /// Y in chrome pixels (already clamped inside the window).
    pub y: f32,
    /// Index of the row whose submenu is open (hover).
    pub sub: Option<usize>,
    /// Chrome size when opened (submenu flipping).
    pub view_w: f32,
    /// Chrome height when opened.
    pub view_h: f32,
}

/// Floating multi-emoji reaction bar (`❤️ 👍 😆 😮 😢 😠`).
#[derive(Clone, Debug, PartialEq)]
pub struct ReactPicker {
    /// Target message.
    pub msg: ChatMsg,
    /// Left coordinate inside the chrome.
    pub x: f32,
    /// Top coordinate inside the chrome.
    pub y: f32,
}

/// Place a `w × h` panel at the cursor like Zalo PC's popover: below-right,
/// flipped above / to the left when it would leave the window, then clamped.
fn place(cursor: f32, size: f32, limit: f32) -> f32 {
    let margin = 4.0;
    if cursor + size + margin <= limit {
        cursor
    } else if cursor - size >= margin {
        cursor - size
    } else {
        (limit - size - margin).max(margin)
    }
}

/// Surfaces that own a Zalo PC context menu.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuKind {
    /// Conversation list row.
    Conv(u64),
    /// Chat bubble.
    Message(ChatMsg),
    /// Contacts person/group row.
    Contact {
        /// Display name.
        name: String,
        /// Group row skips friend-only items.
        is_group: bool,
        /// Friend uid or group thread id. Empty on preview people.
        uid: String,
    },
    /// Rail avatar — name header + `STR_ACCOUNT_MENU`.
    Account,
    /// "Labels ⌄" in the tab bar.
    Filter,
    /// "All ⌄" label filter dropdown in Contacts toolbar.
    ContactFilter,
    /// «…» in the tab bar.
    TabMore,
    /// Right click in the composer.
    Input,
    /// Composer toolbar «…» (`cmp-more`).
    ComposerMore,
    /// Settings `<select>` dropdown (`options` as `(value, label)`).
    SettingSelect {
        /// Pref key (or `None` when informational).
        key: Option<&'static str>,
        /// `(stored_value, display_label)`.
        options: Vec<(String, String)>,
        /// Currently selected stored value.
        current: String,
    },
    /// System tray fallback menu (SNI `ContextMenu` / XEmbed right-click).
    Tray,
    /// Rail gear: account, settings pages, language, logout.
    Gear,
    /// Composer chevron: with / without the Zalo window.
    Screenshot,
    /// Title-bar pill: Light / Dark / System.
    Theme,
    /// Phone card in chat: copy, Zalo call, friend request. The number stays
    /// out of toasts and logs.
    Phone {
        /// Digits as shown on the card.
        phone: String,
    },
}

// `.popover-v3` / `.zmenu-item.md` measured on Zalo PC 26.10.10 (CDP):
// border 1 rgba(0,0,0,.15), padding 8 (items inset 6), radius 8;
// item 36 tall, padding 0 10, radius 4, 4 px apart; separator 1 px
// rgba(0,0,0,.05) with 4 px each side; leading icon 20 px, text at +42;
// trailing «›» 20 px; text 14 px, `--DANGER` #c31818.
const MENU_W: f32 = 226.0;
const ACCOUNT_W: f32 = 298.0;
const ITEM_H: f32 = 36.0;
const ITEM_GAP: f32 = 4.0;
const PANEL_PAD: f32 = 8.0;
const ACCOUNT_HEAD_H: f32 = 35.0;

#[derive(Clone, Debug)]
enum Row {
    Item {
        label: String,
        icon: Option<char>,
        danger: bool,
        enabled: bool,
        checked: bool,
        dot: Option<u32>,
        action: MenuAction,
        sub: Vec<Row>,
    },
    Sep,
}

impl Row {
    fn height(&self) -> f32 {
        match self {
            Row::Item { .. } => ITEM_H,
            Row::Sep => 1.0,
        }
    }
}

fn rows_height(rows: &[Row]) -> f32 {
    let n = rows.len() as f32;
    rows.iter().map(Row::height).sum::<f32>() + ITEM_GAP * (n - 1.0).max(0.0)
}

#[derive(Clone, Debug)]
enum MenuAction {
    None,
    Note(&'static str),
    Pin(u64),
    MuteFor(u64, i64),
    Unmute(u64),
    MarkRead(u64),
    MarkUnread(u64),
    Hide(u64),
    Archive(u64),
    DeleteConv(u64),
    Label(u64, i64),
    React(ChatMsg, &'static str, i64),
    Reply(ChatMsg),
    Copy(String),
    CopyImage(ChatMsg),
    /// Extract text from a photo via [`zocr`] and copy it.
    OcrImage(ChatMsg),
    SaveFile(ChatMsg),
    SaveToCloud(ChatMsg),
    Forward(ChatMsg),
    PinMsg(ChatMsg),
    Star(ChatMsg),
    Details(ChatMsg),
    /// "Unhide Conversation".
    Unhide(u64),
    /// "Manage Labels".
    ManageLabels,
    /// Account menu "Quit Zalo" (the tray keeps the app alive otherwise).
    Quit,
    /// "Select Multiple Messages" starting from this message.
    SelectMulti(ChatMsg),
    Recall(ChatMsg),
    DeleteMsg(ChatMsg),
    OpenChat {
        uid: String,
        name: String,
        is_group: bool,
    },
    Block(String),
    Profile,
    OpenSettings,
    Logout,
    FilterAll,
    OpenSolo(u64),
    InputCut,
    InputCopy,
    InputPaste,
    InputSelectAll,
    FilterStrangers,
    FilterLabel(i64),
    ContactFilterAll,
    ContactFilterLabel(i64),
    MarkAllRead,
    /// Open hidden-chats PIN prompt from the `•••` sidebar menu.
    ShowHidden,
    /// Re-hide unlocked hidden chats from the `•••` sidebar menu.
    LockHidden,
    /// Open/focus main shell window from the tray fallback menu.
    ShowMainWindow,
    /// Hide / minimize the main window to the tray.
    MinimizeToTray,
    /// Open "Send Contact Card" contact picker.
    ComposerNamecard,
    /// Open "Send Bank Account" transfer dialog.
    ComposerBankCard,
    /// Toggle composer formatting & urgency strip.
    ComposerFormat,
    /// Open quick-reply template bar.
    ComposerQuickReply,
    /// Clear draft in `#chatInput`.
    ComposerClearDraft,
    /// Toggle voice recording from composer overflow menu.
    ComposerVoice,
    /// Composer "…" → create reminder.
    ComposerReminder,
    /// Composer "…" → create group note.
    ComposerNote,
    /// Composer "…" → create group poll.
    ComposerPoll,
    /// Apply one dropdown option from `MenuKind::SettingSelect`.
    SettingOption(&'static str, String),
    /// Report conversation.
    ReportConv(u64),
    /// Create reminder from message content.
    CreateReminder(ChatMsg),
    /// Set alias for friend.
    RenameAlias {
        uid: String,
        name: String,
    },
    /// Remove from friends list.
    RemoveFriend {
        uid: String,
        name: String,
    },
    /// Open web link (e.g. Upgrade Business account).
    OpenUrl(&'static str),
    /// Gear and its submenus open Settings on one page.
    OpenSettingsPage(crate::settings::SettingsPage),
    /// Title-bar menu: `light`, `dark`, or `system`.
    SetTheme(&'static str),
    /// Gear language row. `true` is English.
    SetLang(bool),
    /// Remember the screenshot mode. Does not capture.
    ShotWithZalo(bool),
    /// "Version Info".
    About,
    /// Copy a phone number without putting the digits in the toast.
    CopyPhone(String),
    /// Look the number up, then start a 1-1 Zalo call. Does not print it.
    CallPhone(String),
    /// Look the number up, then send a friend request. Does not print it.
    AddFriendPhone(String),
}

fn menu_width(kind: &MenuKind) -> f32 {
    match kind {
        MenuKind::Account => ACCOUNT_W,
        MenuKind::SettingSelect { .. } => 300.0,
        MenuKind::Gear => 260.0,
        MenuKind::Screenshot => 460.0,
        _ => MENU_W,
    }
}

impl ShellView {
    pub(crate) fn open_menu(&mut self, kind: MenuKind, pos: Point<Pixels>, window: &gpui::Window) {
        let (mut x, mut y) = crate::window_frame::window_to_chrome(window, pos);
        let rows = self.rows_for(&kind);
        let width = menu_width(&kind);
        let head = if kind == MenuKind::Account {
            ACCOUNT_HEAD_H
        } else {
            0.0
        };
        let height = rows_height(&rows) + head + PANEL_PAD * 2.0 + 2.0;
        let view = window.viewport_size();
        let csd = if crate::window_frame::CsdCorners::from_window(window).any_pad() {
            zalo_design::WINDOW_SHADOW * 2.0
        } else {
            0.0
        };
        let view_w = f32::from(view.width) - csd;
        let view_h = f32::from(view.height) - csd;
        if kind == MenuKind::Account {
            // `.popover-v3` margin −48 0 0 56 from the avatar: x 68, y 38.
            x = zalo_design::MAIN_TAB_W + 4.0;
            y = zalo_design::LINUX_TITLEBAR_H;
        } else if kind == MenuKind::Gear {
            // Above the rail gear, just to the right of the icon column.
            x = zalo_design::MAIN_TAB_W + 8.0;
            y = (view_h - 12.0 - 56.0 - height).max(4.0);
        } else {
            x = place(x, width, view_w);
            y = place(y, height, view_h);
        }
        self.menu = Some(OpenMenu {
            kind,
            x,
            y,
            sub: None,
            view_w,
            view_h,
        });
        self.sticker_open = false;
        self.forward_of = None;
        self.react_picker = None;
        if matches!(
            self.menu.as_ref().map(|m| &m.kind),
            Some(MenuKind::Conv(_) | MenuKind::Filter | MenuKind::ContactFilter)
        ) {
            self.ensure_labels();
        }
    }

    pub(crate) fn close_menu(&mut self) {
        self.menu = None;
    }

    /// Opens the floating 6-emoji reaction picker above/near `pos`.
    pub(crate) fn open_react_picker(
        &mut self,
        msg: ChatMsg,
        pos: Point<Pixels>,
        window: &gpui::Window,
    ) {
        let (raw_x, raw_y) = crate::window_frame::window_to_chrome(window, pos);
        let view = window.viewport_size();
        let csd = if crate::window_frame::CsdCorners::from_window(window).any_pad() {
            zalo_design::WINDOW_SHADOW * 2.0
        } else {
            0.0
        };
        let view_w = f32::from(view.width) - csd;
        let view_h = f32::from(view.height) - csd;
        let has_my = self.my_reaction(msg.id).is_some();
        let bar_w = if has_my { 256.0 } else { 222.0 };
        let bar_h = 42.0;
        let x = (raw_x - bar_w * 0.5).clamp(8.0, (view_w - bar_w - 8.0).max(8.0));
        let y = if raw_y - bar_h - 10.0 >= 8.0 {
            raw_y - bar_h - 10.0
        } else {
            place(raw_y + 8.0, bar_h, view_h)
        };
        self.menu = None;
        self.forward_of = None;
        self.sticker_open = false;
        self.react_picker = Some(ReactPicker { msg, x, y });
    }

    /// Floating 6-emoji reaction bar overlay (`👍 ❤️ 😆 😮 😢 😠`).
    pub(crate) fn react_picker_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(picker) = self.react_picker.clone() else {
            return div().into_any_element();
        };
        let paper = token_rgb("--layer-background", "#ffffff");
        let active_bg = token_rgb("--B10", "#e5efff");
        let hover_bg = token_rgb("--layer-background-hover", "#f1f2f4");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let line = crate::color::border_subtle();
        let my_type = self.my_reaction(picker.msg.id).map(|r| r.r_type);
        let clear_msg = picker.msg.clone();
        div()
            .id("react-picker-layer")
            .absolute()
            .inset(px(0.))
            .child(
                div()
                    .id("react-picker-dismiss")
                    .absolute()
                    .inset(px(0.))
                    .occlude()
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(|this, _, _, cx| {
                            this.react_picker = None;
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.react_picker = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("react-picker-bar")
                    .absolute()
                    .left(px(picker.x))
                    .top(px(picker.y))
                    .h(px(40.))
                    .px(px(8.))
                    .rounded(px(20.))
                    .bg(rgb(paper))
                    .border_1()
                    .border_color(line)
                    .shadow_lg()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .occlude()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(crate::read_sync::REACTION_PRESETS.iter().enumerate().map(
                        |(idx, &(emoji, r_icon, r_type, label))| {
                            let target = picker.msg.clone();
                            let picked = my_type == Some(r_type);
                            div()
                                .id(("react-pick", idx))
                                .size(px(30.))
                                .rounded(px(15.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(18.))
                                .cursor_pointer()
                                .when(picked, |el| el.bg(rgb(active_bg)))
                                .hover(move |st| st.bg(rgb(hover_bg)))
                                .tooltip(move |_, cx| crate::widgets::tip(label, cx))
                                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                                    if picked {
                                        this.apply_reaction(&target, "", -1);
                                    } else {
                                        this.apply_reaction(&target, r_icon, r_type);
                                    }
                                    this.react_picker = None;
                                    cx.notify();
                                }))
                                .child(emoji)
                        },
                    ))
                    .when(my_type.is_some(), |el| {
                        el.child(
                            div()
                                .id("react-pick-clear")
                                .size(px(26.))
                                .rounded(px(13.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .text_color(rgb(muted))
                                .cursor_pointer()
                                .hover(move |st| st.bg(rgb(hover_bg)))
                                .tooltip(|_, cx| crate::widgets::tip("Bỏ cảm xúc", cx))
                                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                                    this.apply_reaction(&clear_msg, "", -1);
                                    this.react_picker = None;
                                    cx.notify();
                                }))
                                .child("✕"),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Full-window dismiss + the `.popover-v3` panel (and one submenu).
    pub(crate) fn context_menu_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(open) = self.menu.clone() else {
            return div().into_any_element();
        };
        let rows = self.rows_for(&open.kind);
        let width = menu_width(&open.kind);
        let header = (open.kind == MenuKind::Account).then(|| {
            // Same source as the window title: login name, else profile `me`.
            self.display_name
                .clone()
                .filter(|n| !n.is_empty())
                .or_else(|| {
                    self.profiles
                        .map
                        .get(crate::profiles::ME)
                        .map(|p| p.name.clone())
                        .filter(|n| !n.is_empty())
                })
                .unwrap_or_else(|| str_vi("STR_ACCOUNT_MENU", "Hồ sơ của bạn").to_owned())
        });
        let head_h = if header.is_some() {
            ACCOUNT_HEAD_H
        } else {
            0.0
        };
        let sub = open.sub.and_then(|i| match rows.get(i) {
            Some(Row::Item { sub, .. }) if !sub.is_empty() => {
                let top: f32 = rows[..i].iter().map(|r| r.height() + ITEM_GAP).sum();
                let item_y = open.y + head_h + top;
                let sub_h = rows_height(sub) + PANEL_PAD * 2.0 + 2.0;
                // Right of the menu, or left when it would leave the window;
                // aligned to the row, or lifted to stay inside.
                let right = open.x + width - 2.0;
                let sx = if right + MENU_W + 4.0 <= open.view_w {
                    right
                } else {
                    (open.x - MENU_W + 2.0).max(4.0)
                };
                let sy = if item_y + sub_h + 4.0 <= open.view_h {
                    item_y
                } else {
                    (open.view_h - sub_h - 4.0).max(4.0)
                };
                Some((sub.clone(), sx, sy))
            }
            _ => None,
        });
        let main = menu_panel("context-menu", rows, header, width, 0, cx)
            .left(px(open.x))
            .top(px(open.y));
        div()
            .id("context-menu-layer")
            .absolute()
            .inset(px(0.))
            .child(
                div()
                    .id("context-menu-dismiss")
                    .absolute()
                    .inset(px(0.))
                    .occlude()
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(|this, _, _, cx| {
                            this.close_menu();
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_menu();
                        cx.notify();
                    })),
            )
            .child(main)
            .when_some(sub, |el, (rows, sx, sy)| {
                el.child(
                    menu_panel("context-submenu", rows, None, MENU_W, 1000, cx)
                        .left(px(sx))
                        .top(px(sy)),
                )
            })
            .into_any_element()
    }

    /// Picker listing conversations for [`MenuAction::Forward`].
    pub(crate) fn forward_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(msg) = self.forward_of.clone() else {
            return div().into_any_element();
        };
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let hover = token_rgb("--B10", "#f0f7ff");
        let line = crate::color::border_subtle();
        let rows = self.listed_convs();
        div()
            .id("forward-layer")
            .absolute()
            .inset(px(0.))
            .child(
                div()
                    .id("forward-dismiss")
                    .absolute()
                    .inset(px(0.))
                    .occlude()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.forward_of = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("forward-menu")
                    .absolute()
                    .left(px(zalo_design::SIDEBAR_W + 24.0))
                    .top(px(zalo_design::LINUX_TITLEBAR_H + 80.0))
                    .w(px(260.))
                    .max_h(px(360.))
                    .rounded(px(zalo_design::CONTEXT_MENU_RADIUS))
                    .bg(rgb(paper))
                    .border_1()
                    .border_color(line)
                    .shadow_sm()
                    .occlude()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .px(px(14.))
                            .py(px(10.))
                            .text_size(px(13.))
                            .text_color(rgb(ink))
                            .child(str_vi("STR_FORWARD", "Chia sẻ")),
                    )
                    .child(div().h(px(1.)).bg(line))
                    .child(
                        div()
                            .id("forward-list")
                            .flex()
                            .flex_col()
                            .max_h(px(300.))
                            .overflow_y_scroll()
                            .children(rows.into_iter().map(|row| {
                                let dest = row.clone();
                                let body = msg.clone();
                                div()
                                    .id(("forward-row", row.id))
                                    .px(px(14.))
                                    .py(px(8.))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(hover)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.forward_to(&dest, &body);
                                        this.forward_of = None;
                                        cx.notify();
                                    }))
                                    .child(row.name)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Phone card actions. The number is sent to Zalo and never written to a note.
    fn lookup_phone(&mut self, phone: String, call: bool, cx: &mut Context<Self>) {
        if phone.trim().is_empty() {
            return;
        }
        if cfg!(test) || !self.has_session {
            self.set_note("Chưa đăng nhập");
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            self.set_note("Chưa đăng nhập");
            return;
        };
        self.set_note(if call {
            "Đang tìm để gọi…"
        } else {
            "Đang tìm để kết bạn…"
        });
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let value = api
                            .find_user_by_phone(&phone)
                            .await
                            .map_err(|e| e.to_string())?;
                        let person = crate::dialogs::found_user_from(&value)
                            .ok_or_else(|| "Không tìm thấy tài khoản Zalo".to_owned())?;
                        if !call {
                            api.send_friend_request(&person.uid, "Xin chào")
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                        Ok::<_, String>(person)
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match res {
                    Ok(person) if call => view.start_call_uid(person.uid, person.name, false, cx),
                    Ok(_) => view.set_note("Đã gửi lời mời kết bạn"),
                    Err(_) => view.set_note(if call {
                        "Không gọi được số này"
                    } else {
                        "Không gửi được lời mời kết bạn"
                    }),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn rows_for(&self, kind: &MenuKind) -> Vec<Row> {
        match kind {
            MenuKind::Conv(id) => {
                let unread = self.conv(*id).map(|row| self.unread_of(row)).unwrap_or(0);
                let mut rows = conv_rows(
                    *id,
                    self.pinned.contains(id),
                    self.muted.contains(id),
                    self.archived.contains(id),
                    unread > 0,
                    self.label_rows(*id),
                );
                if self.hidden.contains(id) {
                    for r in rows.iter_mut() {
                        if let Row::Item {
                            label,
                            action: action @ MenuAction::Hide(_),
                            ..
                        } = r
                        {
                            *label = "Bỏ ẩn trò chuyện".into();
                            *action = MenuAction::Unhide(*id);
                        }
                    }
                }
                // Without "Split Focused and Other" there is no Other tab.
                if !self.prefs.bool("split", true) {
                    rows.retain(|r| {
                        !matches!(
                            r,
                            Row::Item {
                                action: MenuAction::Archive(_),
                                ..
                            }
                        )
                    });
                }
                if self.solo.is_none() {
                    if let Some(Row::Item {
                        enabled, action, ..
                    }) = rows.first_mut()
                    {
                        *enabled = true;
                        *action = MenuAction::OpenSolo(*id);
                    }
                }
                rows
            }
            MenuKind::Message(msg) => {
                let cloud = self.cloud_id() == Some(msg.conv);
                let my_react = self.my_reaction(msg.id).map(|r| r.r_type);
                message_rows(
                    msg,
                    cloud,
                    self.starred.contains(&msg.id),
                    self.pinned_msgs.contains(&msg.id),
                    my_react,
                )
            }
            MenuKind::Contact {
                name,
                is_group,
                uid,
            } => contact_rows(name, *is_group, uid),
            MenuKind::Account => account_rows(),
            MenuKind::Filter => {
                let mut rows = vec![off(str_vi("STR_FILTER_BY_TAG", "Theo thẻ phân loại"))];
                if self.list_filter != ListFilter::All {
                    rows.push(check(
                        str_vi("STR_ALL", "Tất cả"),
                        false,
                        MenuAction::FilterAll,
                    ));
                }
                if self.stranger_summary().is_some() || self.list_filter == ListFilter::Strangers {
                    rows.push(check(
                        "Tin nhắn từ người lạ",
                        self.list_filter == ListFilter::Strangers,
                        MenuAction::FilterStrangers,
                    ));
                }
                for label in &self.labels {
                    rows.push(Row::Item {
                        label: label.text.clone(),
                        icon: None,
                        danger: false,
                        enabled: true,
                        checked: self.list_filter == ListFilter::Label(label.id),
                        dot: crate::color::parse_hex(&label.color),
                        action: MenuAction::FilterLabel(label.id),
                        sub: Vec::new(),
                    });
                }
                if self.labels.is_empty() {
                    rows.push(off(self.labels_status()));
                }
                rows.push(Row::Sep);
                rows.push(item(
                    str_vi("STR_MANAGE_LABEL", "Quản lý thẻ phân loại"),
                    MenuAction::ManageLabels,
                ));
                rows
            }
            MenuKind::ContactFilter => {
                let mut rows = vec![check(
                    str_vi("STR_ALL", "Tất cả"),
                    self.contacts_label.is_none(),
                    MenuAction::ContactFilterAll,
                )];
                if !self.labels.is_empty() {
                    rows.push(Row::Sep);
                    rows.push(off(str_vi("STR_FILTER_BY_TAG", "Theo thẻ phân loại")));
                    for label in &self.labels {
                        rows.push(Row::Item {
                            label: label.text.clone(),
                            icon: None,
                            danger: false,
                            enabled: true,
                            checked: self.contacts_label == Some(label.id),
                            dot: crate::color::parse_hex(&label.color),
                            action: MenuAction::ContactFilterLabel(label.id),
                            sub: Vec::new(),
                        });
                    }
                } else {
                    rows.push(Row::Sep);
                    rows.push(off(self.labels_status()));
                }
                rows.push(Row::Sep);
                rows.push(item(
                    str_vi("STR_MANAGE_LABEL", "Quản lý thẻ phân loại"),
                    MenuAction::ManageLabels,
                ));
                rows
            }
            MenuKind::Input => vec![
                enabled_if(item("Cắt", MenuAction::InputCut), self.input_has_sel),
                enabled_if(item("Sao chép", MenuAction::InputCopy), self.input_has_sel),
                item("Dán", MenuAction::InputPaste),
                Row::Sep,
                item("Chọn tất cả", MenuAction::InputSelectAll),
            ],
            MenuKind::TabMore => {
                let mut rows = vec![item(
                    str_vi("STR_MARK_AS_READ", "Đánh dấu đã đọc"),
                    MenuAction::MarkAllRead,
                )];
                if !self.hidden.is_empty() {
                    rows.push(Row::Sep);
                    if self.hidden_srv.unlocked {
                        rows.push(item("Ẩn lại trò chuyện", MenuAction::LockHidden));
                    } else {
                        rows.push(item(
                            format!("Hiện trò chuyện ẩn ({})", self.hidden.len()),
                            MenuAction::ShowHidden,
                        ));
                    }
                }
                rows
            }
            MenuKind::ComposerMore => {
                let mut rows = vec![
                    item("Gửi danh thiếp", MenuAction::ComposerNamecard),
                    item("Gửi tài khoản ngân hàng", MenuAction::ComposerBankCard),
                    item("Ghi âm tin nhắn thoại", MenuAction::ComposerVoice),
                    item("Chèn tin nhắn nhanh", MenuAction::ComposerQuickReply),
                    item("Định dạng & Độ khẩn cấp", MenuAction::ComposerFormat),
                    item("Tạo nhắc hẹn", MenuAction::ComposerReminder),
                ];
                if self.selected_conv().is_some_and(|c| c.is_group) {
                    rows.push(item("Tạo ghi chú", MenuAction::ComposerNote));
                    rows.push(item("Tạo bình chọn", MenuAction::ComposerPoll));
                }
                rows.push(Row::Sep);
                rows.push(item(
                    "Xóa nội dung đang soạn",
                    MenuAction::ComposerClearDraft,
                ));
                rows
            }
            MenuKind::SettingSelect {
                key,
                options,
                current,
            } => options
                .iter()
                .map(|(val, label)| {
                    let act = match *key {
                        Some(k) => MenuAction::SettingOption(k, val.clone()),
                        None => MenuAction::None,
                    };
                    check(label.clone(), val == current || label == current, act)
                })
                .collect(),
            MenuKind::Tray => vec![
                item("Mở Zalo", MenuAction::ShowMainWindow),
                item("Ẩn Zalo", MenuAction::MinimizeToTray),
                danger(item("Thoát", MenuAction::Quit)),
            ],
            MenuKind::Gear => gear_rows(self.has_session, self.english),
            MenuKind::Screenshot => {
                let (with_key, without_key) = self.shot_triggers();
                screenshot_rows(self.shot_with_zalo, &with_key, &without_key)
            }
            MenuKind::Theme => theme_rows(
                self.prefs.bool("followSystem", false),
                self.prefs.bool("dark", false),
            ),
            MenuKind::Phone { phone } => vec![
                item(
                    crate::i18n::t("Sao chép số"),
                    MenuAction::CopyPhone(phone.clone()),
                ),
                item(
                    crate::i18n::t("Gọi qua Zalo"),
                    MenuAction::CallPhone(phone.clone()),
                ),
                item(
                    crate::i18n::t("Kết bạn"),
                    MenuAction::AddFriendPhone(phone.clone()),
                ),
            ],
        }
    }

    fn label_rows(&self, conv: u64) -> Vec<Row> {
        let current = self.conv_label(conv);
        let mut rows: Vec<Row> = self
            .labels
            .iter()
            .map(|label| Row::Item {
                label: label.text.clone(),
                icon: None,
                danger: false,
                enabled: true,
                checked: current == Some(label.id),
                dot: crate::color::parse_hex(&label.color),
                action: MenuAction::Label(conv, label.id),
                sub: Vec::new(),
            })
            .collect();
        if rows.is_empty() {
            rows.push(off(self.labels_status()));
        }
        rows.push(Row::Sep);
        rows.push(item(
            str_vi("STR_MANAGE_LABEL", "Quản lý thẻ phân loại"),
            MenuAction::ManageLabels,
        ));
        rows
    }

    fn run_action(
        &mut self,
        action: MenuAction,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let persist = matches!(
            action,
            MenuAction::Pin(_)
                | MenuAction::MuteFor(..)
                | MenuAction::Unmute(_)
                | MenuAction::Archive(_)
                | MenuAction::Hide(_)
                | MenuAction::DeleteConv(_)
        );
        self.run_action_inner(action, window, cx);
        if persist {
            self.save_conv_flags();
        }
    }

    fn run_action_inner(
        &mut self,
        action: MenuAction,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            MenuAction::None => {}
            MenuAction::Note(text) => self.set_note(text),
            MenuAction::MuteFor(id, secs) => {
                self.muted.insert(id);
                if let Some(conv) = self.conv(id).cloned() {
                    self.maybe_live(LiveAction::Mute {
                        thread_id: conv.thread_id,
                        is_group: conv.is_group,
                        mute: true,
                        duration: secs,
                    });
                }
            }
            MenuAction::Unmute(id) => {
                self.muted.remove(&id);
                if let Some(conv) = self.conv(id).cloned() {
                    self.maybe_live(LiveAction::Mute {
                        thread_id: conv.thread_id,
                        is_group: conv.is_group,
                        mute: false,
                        duration: -1,
                    });
                }
            }
            MenuAction::MarkUnread(id) => {
                self.marked_read.remove(&id);
                if let Some(conv) = self.convs.iter_mut().find(|row| row.id == id) {
                    conv.unread = conv.unread.max(1);
                }
                if let Some(item) = self.last_thread_ref(id) {
                    self.maybe_live(LiveAction::MarkUnread { item });
                }
            }
            MenuAction::Label(id, label) => self.toggle_conv_label(id, label, cx),
            MenuAction::ManageLabels => self.open_labels(cx),
            MenuAction::Quit => {
                self.clear_session_if_unremembered();
                zalo_shell::stop_instance_listener(zalo_shell::GPUI_APP_ID);
                cx.quit();
            }
            MenuAction::CopyImage(msg) => self.copy_image(&msg, cx),
            MenuAction::OcrImage(msg) => self.run_viewer_ocr(msg, Some(window), cx),
            MenuAction::SaveFile(msg) => self.save_media(&msg),
            MenuAction::SaveToCloud(msg) => {
                if let Some(cloud) = self.cloud_id().and_then(|id| self.conv(id).cloned()) {
                    self.forward_to(&cloud, &msg);
                }
            }
            MenuAction::PinMsg(msg) => {
                let pin = if self.pinned_msgs.remove(&msg.id) {
                    false
                } else {
                    self.pinned_msgs.insert(msg.id);
                    true
                };
                if let Some(conv) = self.conv(msg.conv).cloned().filter(|c| c.is_group) {
                    if let Some(global) = msg.global_msg_id {
                        if pin {
                            self.maybe_live(LiveAction::PinBoard {
                                thread_id: conv.thread_id,
                                topic_id: global.to_string(),
                                item_type: 1,
                            });
                        } else {
                            self.maybe_live(LiveAction::UnpinBoard {
                                thread_id: conv.thread_id,
                                topic_id: global.to_string(),
                                board_version: 0,
                            });
                        }
                    }
                }
            }
            MenuAction::Star(msg) => {
                if !self.starred.remove(&msg.id) {
                    self.starred.insert(msg.id);
                }
                self.save_starred();
            }
            MenuAction::SelectMulti(msg) => {
                self.select_msgs = Some((msg.conv, vec![msg.id]));
                self.reply_to = None;
            }
            MenuAction::Details(msg) => {
                self.dialog = Some(crate::dialogs::Dialog::Details(msg));
            }
            MenuAction::FilterAll => self.list_filter = ListFilter::All,
            MenuAction::InputCut => self
                .composer
                .update(cx, |i, cx| i.cut_selection(window, cx)),
            MenuAction::InputCopy => self.composer.update(cx, |i, cx| i.copy_selection(cx)),
            MenuAction::InputPaste => self
                .composer
                .update(cx, |i, cx| i.paste_clipboard(window, cx)),
            MenuAction::InputSelectAll => self.composer.update(cx, |i, cx| i.select_everything(cx)),
            MenuAction::OpenSolo(id) => self.open_solo(id, cx),
            MenuAction::FilterStrangers => self.list_filter = ListFilter::Strangers,
            MenuAction::FilterLabel(id) => self.list_filter = ListFilter::Label(id),
            MenuAction::ContactFilterAll => self.contacts_label = None,
            MenuAction::ContactFilterLabel(id) => self.contacts_label = Some(id),
            MenuAction::MarkAllRead => {
                let mut mark_threads = Vec::new();
                for conv in &mut self.convs {
                    self.marked_read.insert(conv.id);
                    conv.unread = 0;
                    if conv.live {
                        let at = self
                            .messages
                            .get(&conv.id)
                            .and_then(|l| l.last())
                            .map(|m| m.at_ms)
                            .unwrap_or(0)
                            .max(crate::data::now_ms());
                        mark_threads.push((conv.thread_id.clone(), at));
                    }
                }
                for (t, at) in mark_threads {
                    self.mark_thread_read_pref(&t, at);
                }
            }
            MenuAction::ShowHidden => {
                self.open_pin(crate::privacy::PinMode::Unlock { conv: None }, cx);
            }
            MenuAction::LockHidden => self.lock_hidden_chats(),
            MenuAction::Pin(id) => {
                let pin = !self.pinned.contains(&id);
                if pin {
                    self.pinned.insert(id);
                } else {
                    self.pinned.remove(&id);
                }
                if let Some(conv) = self.conv(id).cloned() {
                    self.maybe_live(LiveAction::Pin {
                        thread_id: conv.thread_id,
                        pin,
                    });
                }
            }
            MenuAction::MarkRead(id) => {
                self.marked_read.insert(id);
                let mut live = None;
                let last_at = self
                    .messages
                    .get(&id)
                    .and_then(|l| l.last())
                    .map(|m| m.at_ms)
                    .unwrap_or(0)
                    .max(crate::data::now_ms());
                if let Some(conv) = self.convs.iter_mut().find(|row| row.id == id) {
                    conv.unread = 0;
                    if conv.live {
                        live = Some((conv.thread_id.clone(), conv.is_group));
                    }
                }
                if let Some((t, is_group)) = live {
                    self.mark_thread_read_pref(&t, last_at);
                    self.maybe_live(LiveAction::RemoveUnread {
                        thread_id: t,
                        is_group,
                        ts: last_at,
                    });
                }
            }
            MenuAction::Hide(id) => self.hide_conv(id, cx),
            MenuAction::Unhide(id) => self.unhide_conv(id, cx),
            MenuAction::Archive(id) => {
                let archive = !self.archived.contains(&id);
                if archive {
                    self.archived.insert(id);
                    self.set_note(str_vi("STR_MOVE_TO_ARCHIVED_CHAT", "Chuyển sang mục Khác"));
                } else {
                    self.archived.remove(&id);
                    self.set_note(str_vi(
                        "STR_REMOVE_FROM_ARCHIVED_CHAT",
                        "Chuyển sang mục Ưu tiên",
                    ));
                }
                if let Some(conv) = self.conv(id).cloned() {
                    self.maybe_live(LiveAction::Archive {
                        thread_id: conv.thread_id,
                        archive,
                    });
                }
            }
            MenuAction::DeleteConv(id) => {
                if let Some(item) = self.last_thread_ref(id) {
                    self.maybe_live(LiveAction::DeleteConv {
                        item,
                        client_id: bridge::client_id(),
                    });
                }
                self.removed.insert(id);
                if self.selected == Some(id) {
                    self.selected = None;
                    self.info_open = false;
                }
                self.set_note(str_vi("STR_DELETE_CONVERSATION", "Xóa hội thoại"));
            }
            MenuAction::React(msg, r_icon, r_type) => {
                self.apply_reaction(&msg, r_icon, r_type);
            }
            MenuAction::Reply(msg) => {
                self.reply_to = Some(msg);
                let handle = self.composer.read(cx).focus_handle(cx);
                window.focus(&handle);
            }
            MenuAction::Copy(text) => {
                crate::clip::copy_text(cx, text);
                self.set_note(str_vi("STR_COPY_0", "Sao chép"));
            }
            MenuAction::CopyPhone(phone) => {
                crate::clip::copy_text(cx, phone);
                self.set_note("Đã sao chép số điện thoại");
            }
            MenuAction::CallPhone(phone) => self.lookup_phone(phone, true, cx),
            MenuAction::AddFriendPhone(phone) => self.lookup_phone(phone, false, cx),
            MenuAction::Forward(msg) => {
                self.open_share(msg, cx);
            }
            MenuAction::Recall(msg) => {
                self.apply_recall(&msg);
            }
            MenuAction::DeleteMsg(msg) => {
                self.apply_delete_msg(&msg);
            }
            MenuAction::OpenChat {
                uid,
                name,
                is_group,
            } => {
                self.open_chat_or_note(
                    &uid,
                    &name,
                    is_group,
                    "Danh bạ — chưa có hội thoại",
                    window,
                    cx,
                );
            }
            MenuAction::Block(uid) => {
                if uid.is_empty() {
                    self.set_note("Không có mã người dùng để chặn");
                } else {
                    let name = self
                        .profiles
                        .map
                        .get(&uid)
                        .map(|p| p.name.clone())
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| uid.clone());
                    match self.privacy.blocked.as_mut() {
                        Some(list) => {
                            if !list.iter().any(|(id, _)| id == &uid) {
                                list.push((uid.clone(), name));
                            }
                        }
                        None => self.privacy.blocked = Some(vec![(uid.clone(), name)]),
                    }
                    self.maybe_live(LiveAction::Block { uid: uid.clone() });
                    self.set_note(str_vi("STR_BLOCK_MSG_CALL", "Đã chặn tin nhắn và cuộc gọi"));
                }
            }
            MenuAction::Profile => self.open_profile_dialog(cx),
            MenuAction::OpenSettings => {
                self.set_rail(crate::rail::Rail::Settings);
            }
            MenuAction::ShowMainWindow => window.activate_window(),
            MenuAction::MinimizeToTray => {
                if zalo_shell::tray_active() {
                    window.minimize_window();
                    crate::background::tell_tray_once();
                } else {
                    self.set_note("Chưa phát hiện khay hệ thống để thu nhỏ");
                }
            }
            MenuAction::Logout => {
                self.leave_to_login(window, cx, true);
            }
            MenuAction::ComposerNamecard => {
                self.sticker_open = false;
                self.quick_menu = false;
                self.open_namecard_picker(cx);
            }
            MenuAction::ComposerBankCard => {
                self.sticker_open = false;
                self.quick_menu = false;
                self.open_bank_card_dialog(cx);
            }
            MenuAction::ComposerFormat => {
                self.sticker_open = false;
                self.quick_menu = false;
                self.format_open = !self.format_open;
            }
            MenuAction::ComposerQuickReply => {
                self.sticker_open = false;
                self.quick_menu = true;
            }
            MenuAction::ComposerClearDraft => {
                self.composer.update(cx, |input, cx| input.clear(cx));
                self.stickers.inline = None;
                self.stickers.suggest_items.clear();
                self.urgency = 0;
            }
            MenuAction::ComposerVoice => {
                self.start_voice_record(cx);
            }
            MenuAction::ComposerReminder => {
                if let Some(id) = self.selected {
                    self.open_board_dialog(id, crate::board::BoardTab::CreateReminder, cx);
                }
            }
            MenuAction::ComposerNote => {
                if let Some(id) = self.selected {
                    self.open_board_dialog(id, crate::board::BoardTab::CreateNote, cx);
                }
            }
            MenuAction::ComposerPoll => {
                if let Some(id) = self.selected {
                    self.open_board_dialog(id, crate::board::BoardTab::CreatePoll, cx);
                }
            }
            MenuAction::SettingOption(key, value) => {
                self.set_pref(key, serde_json::Value::String(value), window, cx);
            }
            MenuAction::ReportConv(id) => self.open_report(id, cx),
            MenuAction::CreateReminder(msg) => {
                self.open_board_reminder_with_text(msg.conv, &msg.text, cx);
            }
            MenuAction::RenameAlias { uid, name } => self.open_alias(uid, name, cx),
            MenuAction::RemoveFriend { uid, name } => self.open_remove_friend(uid, name),
            MenuAction::OpenUrl(url) => cx.open_url(url),
            MenuAction::OpenSettingsPage(page) => {
                self.settings_page = page;
                self.set_rail(crate::rail::Rail::Settings);
            }
            MenuAction::SetTheme(mode) => {
                self.set_pref(
                    "theme",
                    serde_json::Value::String(mode.to_owned()),
                    window,
                    cx,
                );
            }
            MenuAction::SetLang(en) => {
                let label = if en { "English" } else { "Tiếng Việt" };
                self.set_pref(
                    "langLabel",
                    serde_json::Value::String(label.to_owned()),
                    window,
                    cx,
                );
            }
            MenuAction::ShotWithZalo(on) => {
                // Electron `onMenuItemClick`: save `z_capzalo`, then start the
                // capture after 100ms. `on == false` hides the Zalo window
                // (plugin `captureMode === false`).
                self.shot_with_zalo = on;
                self.prefs.set("shotWithZalo", serde_json::Value::Bool(on));
                let wh = window.window_handle();
                cx.spawn(async move |this, cx| {
                    gpui::Timer::after(std::time::Duration::from_millis(100)).await;
                    let _ = this.update(cx, |view, cx| {
                        view.capture_screenshot_mode(!on, Some(wh), cx);
                    });
                })
                .detach();
            }
            MenuAction::About => {
                self.set_note("Zalo cho Linux theo Zalo PC 26.10.10");
            }
        }
    }

    fn last_thread_ref(&self, id: u64) -> Option<zalo_client::ThreadMsgRef> {
        let conv = self.conv(id)?;
        build_thread_msg_ref(conv, self.messages_of(id), self.self_uid())
    }

    /// Returns this user's active reaction on `msg_id`, if any.
    pub(crate) fn my_reaction(&self, msg_id: u64) -> Option<&crate::read_sync::MsgReaction> {
        let uid = self.self_uid();
        let uin = self.uin.as_deref();
        self.reactions.get(&msg_id)?.iter().find(|r| {
            r.sender_uid == "me"
                || uid.is_some_and(|u| r.sender_uid == u)
                || uin.is_some_and(|u| r.sender_uid == u)
        })
    }

    /// Sends or clears a specific Zalo reaction on `msg` and updates local state immediately.
    pub(crate) fn apply_reaction(&mut self, msg: &ChatMsg, r_icon: &str, r_type: i64) {
        let cleared = r_type < 0;
        let sender = self
            .self_uid()
            .or(self.uin.as_deref())
            .unwrap_or("me")
            .to_owned();
        crate::read_sync::update_reaction(
            &mut self.reactions,
            msg.id,
            &sender,
            r_icon,
            r_type,
            cleared,
        );
        let has_any = self.reactions.get(&msg.id).is_some_and(|v| !v.is_empty());
        if let Some(list) = self.messages.get_mut(&msg.conv) {
            if let Some(found) = list.iter_mut().find(|row| row.id == msg.id) {
                found.liked = has_any;
            }
        }
        if let Some(conv) = self.conv(msg.conv).cloned() {
            if let (Some(global), Some(cli)) = (msg.global_msg_id, msg.cli_msg_id) {
                self.maybe_live(LiveAction::React {
                    thread_id: conv.thread_id,
                    is_group: conv.is_group,
                    msg_id: global,
                    cli_msg_id: cli,
                    msg_type: bridge::msg_type_for(msg.kind),
                    reaction: zalo_client::Reaction {
                        r_type,
                        r_icon: r_icon.to_owned(),
                    },
                    client_id: bridge::client_id(),
                });
            }
        }
    }

    pub(crate) fn apply_like(&mut self, msg: &ChatMsg) {
        if self.my_reaction(msg.id).is_some() {
            self.apply_reaction(msg, "", -1);
        } else {
            self.apply_reaction(msg, "/-strong", 3);
        }
    }

    fn apply_recall(&mut self, msg: &ChatMsg) {
        let recalled = str_vi("STR_UNDO_MESSAGE", "Tin nhắn đã thu hồi").to_owned();
        if let Some(list) = self.messages.get_mut(&msg.conv) {
            if let Some(found) = list.iter_mut().find(|row| row.id == msg.id) {
                found.recalled = true;
                found.text = recalled.clone();
            }
        }
        if let Some(conv) = self.conv(msg.conv).cloned() {
            if let (Some(global), Some(cli)) = (msg.global_msg_id, msg.cli_msg_id) {
                self.maybe_live(LiveAction::Undo {
                    thread_id: conv.thread_id,
                    is_group: conv.is_group,
                    msg_id: global,
                    client_id: bridge::client_id(),
                    cli_msg_id_undo: cli,
                });
            }
        }
    }

    pub(crate) fn apply_delete_msg(&mut self, msg: &ChatMsg) {
        if let Some(list) = self.messages.get_mut(&msg.conv) {
            list.retain(|row| row.id != msg.id);
        }
        if msg.id > 0 && data::live_allowed(&msg.thread_id) {
            data::persist_delete_message(msg.id as i64);
        }
        if let Some(conv) = self.conv(msg.conv).cloned() {
            if let Some(item) = build_delete_item(&conv, msg, self.self_uid()) {
                self.maybe_live(LiveAction::Delete {
                    thread_id: conv.thread_id,
                    is_group: conv.is_group,
                    cli_msg_id: bridge::client_id() as i64,
                    item,
                });
            }
        }
        self.set_note(str_vi("STR_DELETE_MSG", "Xóa"));
    }

    pub(crate) fn forward_to(&mut self, dest: &data::ChatConv, src: &ChatMsg) {
        let now = data::now_ms();
        let id = self.next_msg_id;
        self.next_msg_id += 1;
        let cli = now.max(1);
        self.push_message(ChatMsg::text_msg(
            id,
            dest.id,
            dest.thread_id.clone(),
            true,
            str_vi("STR_YOU", "Bạn").to_owned(),
            self.self_uid().unwrap_or_default().to_owned(),
            src.text.clone(),
            data::vn_hm(now),
            now,
            Some(cli),
            None,
        ));
        self.select_conv(dest.id);
        if dest.live {
            self.maybe_live(LiveAction::Forward {
                dest_id: dest.thread_id.clone(),
                is_group: dest.is_group,
                text: src.text.clone(),
                client_id: cli as u64,
            });
        }
    }
}

mod mi {
    //! Leading menu icons (`fa-*_24_Line`, live font 6835d6).
    pub const QUOTE: char = '\u{ECED}';
    pub const SHARE: char = '\u{ED2A}';
    pub const COPY: char = '\u{EAC5}';
    pub const PHOTO: char = '\u{ECC5}';
    pub const DOWNLOAD: char = '\u{EAE1}';
    pub const PIN: char = '\u{ECCB}';
    pub const STAR: char = '\u{ED73}';
    pub const MULTI: char = '\u{EC68}';
    pub const INFO: char = '\u{EC2A}';
    pub const MORE: char = '\u{EC6E}';
    pub const DOCS: char = '\u{EC6E}';
    pub const REMINDER: char = '\u{ED02}';
    pub const RECALL: char = '\u{ECF6}';
    pub const TRASH: char = '\u{EDCF}';
    pub const ARROW: char = '\u{EA1C}';
    pub const CHECK: char = '\u{EA7C}';
}

fn enabled_if(mut row: Row, on: bool) -> Row {
    if let Row::Item { enabled, .. } = &mut row {
        *enabled = on;
    }
    row
}

fn item(label: impl Into<String>, action: MenuAction) -> Row {
    Row::Item {
        label: label.into(),
        icon: None,
        danger: false,
        enabled: true,
        checked: false,
        dot: None,
        action,
        sub: Vec::new(),
    }
}

fn icon_item(icon: char, label: impl Into<String>, action: MenuAction) -> Row {
    match item(label, action) {
        Row::Item {
            label,
            danger,
            enabled,
            checked,
            dot,
            action,
            sub,
            ..
        } => Row::Item {
            label,
            icon: Some(icon),
            danger,
            enabled,
            checked,
            dot,
            action,
            sub,
        },
        sep => sep,
    }
}

fn danger(row: Row) -> Row {
    match row {
        Row::Item {
            label,
            icon,
            enabled,
            checked,
            dot,
            action,
            sub,
            ..
        } => Row::Item {
            label,
            icon,
            danger: true,
            enabled,
            checked,
            dot,
            action,
            sub,
        },
        sep => sep,
    }
}

fn with_sub(row: Row, rows: Vec<Row>) -> Row {
    match row {
        Row::Item {
            label,
            icon,
            danger,
            enabled,
            checked,
            dot,
            action,
            ..
        } => Row::Item {
            label,
            icon,
            danger,
            enabled,
            checked,
            dot,
            action,
            sub: rows,
        },
        sep => sep,
    }
}

fn check(label: impl Into<String>, on: bool, action: MenuAction) -> Row {
    match item(label, action) {
        Row::Item {
            label,
            icon,
            danger,
            enabled,
            dot,
            action,
            sub,
            ..
        } => Row::Item {
            label,
            icon,
            danger,
            enabled,
            checked: on,
            dot,
            action,
            sub,
        },
        sep => sep,
    }
}

/// Greyed row: the Linux client does not implement it yet.
fn off(label: impl Into<String>) -> Row {
    match item(label, MenuAction::Note("Bản Linux chưa hỗ trợ mục này")) {
        Row::Item {
            label,
            icon,
            danger,
            checked,
            dot,
            action,
            sub,
            ..
        } => Row::Item {
            label,
            icon,
            danger,
            enabled: false,
            checked,
            dot,
            action,
            sub,
        },
        sep => sep,
    }
}

/// Seconds until the next 08:00 in Vietnam time (`STR_MUTE_TO_8_AM`).
fn secs_until_8am(now_ms: i64) -> i64 {
    let day_ms = 86_400_000_i64;
    let vn = now_ms + 7 * 3_600_000;
    let into_day = vn.rem_euclid(day_ms);
    let eight = 8 * 3_600_000;
    let wait = if into_day < eight {
        eight - into_day
    } else {
        day_ms - into_day + eight
    };
    wait / 1000
}

fn conv_rows(
    id: u64,
    pinned: bool,
    muted: bool,
    archived: bool,
    unread: bool,
    labels: Vec<Row>,
) -> Vec<Row> {
    let mute = if muted {
        item("Bật thông báo", MenuAction::Unmute(id))
    } else {
        with_sub(
            item("Tắt thông báo", MenuAction::None),
            vec![
                item("Trong 1 giờ", MenuAction::MuteFor(id, 3600)),
                item("Trong 4 giờ", MenuAction::MuteFor(id, 4 * 3600)),
                item(
                    "Cho đến 8:00 AM",
                    MenuAction::MuteFor(id, secs_until_8am(data::now_ms())),
                ),
                item("Cho đến khi được mở lại", MenuAction::MuteFor(id, -1)),
            ],
        )
    };
    vec![
        off("Mở cửa sổ riêng"),
        item(
            if pinned {
                "Bỏ ghim hội thoại"
            } else {
                "Ghim hội thoại"
            },
            MenuAction::Pin(id),
        ),
        Row::Sep,
        item(
            if archived {
                "Chuyển sang mục Ưu tiên"
            } else {
                "Chuyển sang mục Khác"
            },
            MenuAction::Archive(id),
        ),
        with_sub(item(crate::i18n::t("Phân loại"), MenuAction::None), labels),
        if unread {
            item("Đánh dấu đã đọc", MenuAction::MarkRead(id))
        } else {
            item("Đánh dấu chưa đọc", MenuAction::MarkUnread(id))
        },
        Row::Sep,
        mute,
        item("Ẩn trò chuyện", MenuAction::Hide(id)),
        Row::Sep,
        danger(item("Xóa hội thoại", MenuAction::DeleteConv(id))),
        Row::Sep,
        item("Báo xấu", MenuAction::ReportConv(id)),
    ]
}

fn message_rows(
    msg: &ChatMsg,
    in_cloud: bool,
    starred: bool,
    pinned: bool,
    my_react: Option<i64>,
) -> Vec<Row> {
    use crate::media::MsgKind;
    let delete_for_me = danger(icon_item(
        mi::TRASH,
        "Xóa chỉ ở phía tôi",
        MenuAction::DeleteMsg(msg.clone()),
    ));
    if matches!(msg.kind, MsgKind::Call | MsgKind::MissedCall) {
        return vec![
            icon_item(
                mi::MULTI,
                "Chọn nhiều tin nhắn",
                MenuAction::SelectMulti(msg.clone()),
            ),
            icon_item(mi::INFO, "Xem chi tiết", MenuAction::Details(msg.clone())),
            Row::Sep,
            delete_for_me,
        ];
    }
    let mut react_sub: Vec<Row> = crate::read_sync::REACTION_PRESETS
        .iter()
        .map(|&(emoji, r_icon, r_type, label)| {
            check(
                format!("{emoji} {label}"),
                my_react == Some(r_type),
                MenuAction::React(msg.clone(), r_icon, r_type),
            )
        })
        .collect();
    if my_react.is_some() {
        react_sub.push(Row::Sep);
        react_sub.push(item("Bỏ cảm xúc", MenuAction::React(msg.clone(), "", -1)));
    }
    let mut rows = vec![
        icon_item(mi::QUOTE, "Trả lời", MenuAction::Reply(msg.clone())),
        icon_item(mi::SHARE, "Chia sẻ", MenuAction::Forward(msg.clone())),
        with_sub(
            icon_item(mi::STAR, "Thả cảm xúc", MenuAction::None),
            react_sub,
        ),
        Row::Sep,
    ];
    let more: Vec<Row> = match msg.kind {
        MsgKind::Photo | MsgKind::Gif | MsgKind::Doodle | MsgKind::Sticker => {
            rows.push(icon_item(
                mi::PHOTO,
                "Copy hình ảnh",
                MenuAction::CopyImage(msg.clone()),
            ));
            rows.push(icon_item(
                mi::DOWNLOAD,
                "Lưu về máy",
                MenuAction::SaveFile(msg.clone()),
            ));
            let mut sub = Vec::new();
            if matches!(msg.kind, MsgKind::Photo | MsgKind::Doodle | MsgKind::Gif) {
                sub.push(icon_item(
                    mi::COPY,
                    "Quét chữ (OCR)",
                    MenuAction::OcrImage(msg.clone()),
                ));
            }
            sub.push(icon_item(
                mi::DOCS,
                "Lưu vào My Documents",
                MenuAction::SaveToCloud(msg.clone()),
            ));
            sub
        }
        MsgKind::File => {
            rows.push(icon_item(
                mi::DOWNLOAD,
                "Lưu về máy",
                MenuAction::SaveFile(msg.clone()),
            ));
            vec![icon_item(
                mi::DOCS,
                "Lưu vào My Documents",
                MenuAction::SaveToCloud(msg.clone()),
            )]
        }
        MsgKind::Link => {
            rows.push(icon_item(
                mi::COPY,
                "Copy link",
                MenuAction::Copy(msg.href.clone()),
            ));
            vec![icon_item(
                mi::REMINDER,
                "Tạo nhắc hẹn",
                MenuAction::CreateReminder(msg.clone()),
            )]
        }
        _ => {
            let link = crate::media::first_link(&msg.text);
            match link {
                Some(url) => rows.push(icon_item(mi::COPY, "Copy link", MenuAction::Copy(url))),
                None => rows.push(icon_item(
                    mi::COPY,
                    "Copy tin nhắn",
                    MenuAction::Copy(msg.text.clone()),
                )),
            }
            vec![icon_item(
                mi::REMINDER,
                "Tạo nhắc hẹn",
                MenuAction::CreateReminder(msg.clone()),
            )]
        }
    };
    rows.push(Row::Sep);
    if !in_cloud {
        rows.push(icon_item(
            mi::PIN,
            if pinned {
                "Bỏ ghim tin nhắn"
            } else {
                "Ghim tin nhắn"
            },
            MenuAction::PinMsg(msg.clone()),
        ));
    }
    rows.push(icon_item(
        mi::STAR,
        if starred {
            "Bỏ đánh dấu tin nhắn"
        } else {
            "Đánh dấu tin nhắn"
        },
        MenuAction::Star(msg.clone()),
    ));
    rows.push(icon_item(
        mi::MULTI,
        "Chọn nhiều tin nhắn",
        MenuAction::SelectMulti(msg.clone()),
    ));
    rows.push(icon_item(
        mi::INFO,
        "Xem chi tiết",
        MenuAction::Details(msg.clone()),
    ));
    rows.push(with_sub(
        icon_item(mi::MORE, "Tuỳ chọn khác", MenuAction::None),
        more,
    ));
    rows.push(Row::Sep);
    if msg.from_me && !msg.recalled {
        rows.push(danger(icon_item(
            mi::RECALL,
            "Thu hồi",
            MenuAction::Recall(msg.clone()),
        )));
    }
    rows.push(delete_for_me);
    rows
}

fn contact_rows(name: &str, is_group: bool, uid: &str) -> Vec<Row> {
    let mut rows = vec![item(
        "Nhắn tin",
        MenuAction::OpenChat {
            uid: uid.to_owned(),
            name: name.to_owned(),
            is_group,
        },
    )];
    if !is_group {
        rows.push(item(
            "Đặt tên gợi nhớ",
            MenuAction::RenameAlias {
                uid: uid.to_owned(),
                name: name.to_owned(),
            },
        ));
        rows.push(item(
            "Chặn tin nhắn và cuộc gọi",
            MenuAction::Block(uid.to_owned()),
        ));
        rows.push(Row::Sep);
        rows.push(danger(item(
            "Xóa khỏi danh sách bạn bè",
            MenuAction::RemoveFriend {
                uid: uid.to_owned(),
                name: name.to_owned(),
            },
        )));
    }
    rows
}

fn gear_rows(logged_in: bool, english: bool) -> Vec<Row> {
    let lang = with_sub(
        icon_item(
            '\u{EC2E}',
            str_vi("STR_MENU_LANGUAGE", "Ngôn ngữ"),
            MenuAction::None,
        ),
        vec![
            check("Tiếng Việt", !english, MenuAction::SetLang(false)),
            check("English", english, MenuAction::SetLang(true)),
        ],
    );
    if !logged_in {
        return vec![
            item(
                str_vi("STR_MENU_ABOUT_ZALO", "Giới thiệu"),
                MenuAction::About,
            ),
            Row::Sep,
            lang,
            danger(item(str_vi("STR_MENU_QUIT", "Thoát"), MenuAction::Quit)),
        ];
    }
    vec![
        icon_item(
            crate::fonts::ic::USER,
            str_vi("STR_ACCOUNT", "Thông tin tài khoản"),
            MenuAction::Profile,
        ),
        icon_item(
            crate::fonts::ic::SETTING,
            str_vi("STR_MENU_SETTING", "Cài đặt"),
            MenuAction::OpenSettingsPage(crate::settings::SettingsPage::General),
        ),
        Row::Sep,
        with_sub(
            icon_item(
                crate::fonts::ic::STORAGE,
                str_vi("STR_DATA", "Dữ liệu"),
                MenuAction::None,
            ),
            vec![
                item(
                    str_vi("STR_SYNC_AND_BACKUP_CONFIGURATION", "Đồng bộ và sao lưu"),
                    MenuAction::OpenSettingsPage(crate::settings::SettingsPage::Sync),
                ),
                item(
                    str_vi("STR_RESOURCE_MANAGE", "Quản lý dữ liệu"),
                    MenuAction::OpenSettingsPage(crate::settings::SettingsPage::Data),
                ),
            ],
        ),
        lang,
        with_sub(
            icon_item(
                '\u{EA7A}',
                str_vi("STR_MENU_HELP", "Hỗ trợ"),
                MenuAction::None,
            ),
            vec![
                item(
                    str_vi("STR_VERSION_INFO", "Thông tin phiên bản"),
                    MenuAction::About,
                ),
                item(
                    str_vi("STR_FEEDBACK", "Liên hệ"),
                    MenuAction::Note("Liên hệ hỗ trợ qua Zalo trên điện thoại."),
                ),
            ],
        ),
        Row::Sep,
        item(
            str_vi("STR_MENU_HOTKEYS", "Phím tắt"),
            MenuAction::OpenSettingsPage(crate::settings::SettingsPage::Utilities),
        ),
        Row::Sep,
        danger(item(
            str_vi("STR_MENU_LOGOUT", "Đăng xuất"),
            MenuAction::Logout,
        )),
    ]
}

fn theme_rows(follow_system: bool, dark: bool) -> Vec<Row> {
    vec![
        check(
            "Sáng",
            !follow_system && !dark,
            MenuAction::SetTheme("light"),
        ),
        check("Tối", !follow_system && dark, MenuAction::SetTheme("dark")),
        check("Hệ Thống", follow_system, MenuAction::SetTheme("system")),
    ]
}

fn screenshot_rows(with_zalo: bool, with_key: &str, without_key: &str) -> Vec<Row> {
    let with_label = str_vi(
        "STR_SCREENSHOT_WITH_ZALO_0",
        "Chụp kèm với cửa sổ Zalo ($0$)",
    )
    .replace("$0$", &crate::shortcuts::format_trigger(with_key));
    let without_label = str_vi(
        "STR_SCREENSHOT_WITHOUT_ZALO_0",
        "Chụp KHÔNG kèm cửa sổ Zalo ($0$)",
    )
    .replace("$0$", &crate::shortcuts::format_trigger(without_key));
    // Electron menu icons: Screenshot-Z (with the window) and Screenshot (without).
    vec![
        shot_check(
            crate::fonts::ic::TB_SHOT,
            with_label,
            with_zalo,
            MenuAction::ShotWithZalo(true),
        ),
        shot_check(
            crate::fonts::ic::TB_SHOT_PLAIN,
            without_label,
            !with_zalo,
            MenuAction::ShotWithZalo(false),
        ),
    ]
}

fn shot_check(icon: char, label: impl Into<String>, on: bool, action: MenuAction) -> Row {
    match check(label, on, action) {
        Row::Item {
            label,
            danger,
            enabled,
            checked,
            dot,
            action,
            sub,
            ..
        } => Row::Item {
            label,
            icon: Some(icon),
            danger,
            enabled,
            checked,
            dot,
            action,
            sub,
        },
        sep => sep,
    }
}

fn account_rows() -> Vec<Row> {
    // Avatar popover on Zalo PC: name, upgrade (Extend_24_Line on the right),
    // "Your Profile", "Settings", separator, "Log out".
    // "Sign in again" stays on the session banner. "Quit Zalo" stays on the tray.
    vec![
        Row::Sep,
        item(
            str_vi("STR_UPGRADE_ACCOUNT", "Nâng cấp tài khoản"),
            MenuAction::OpenUrl("https://business.zbox.vn/nang-cap-business-pro/"),
        ),
        item(
            str_vi("STR_ACCOUNT_MENU", "Hồ sơ của bạn"),
            MenuAction::Profile,
        ),
        item(
            str_vi("STR_MENU_SETTING", "Cài đặt"),
            MenuAction::OpenSettings,
        ),
        Row::Sep,
        item(str_vi("STR_MENU_LOGOUT", "Đăng xuất"), MenuAction::Logout),
    ]
}

/// One `.popover-v3` panel. `base` keeps element ids unique per panel.
fn menu_panel(
    id: &'static str,
    rows: Vec<Row>,
    header: Option<String>,
    width: f32,
    base: usize,
    cx: &mut Context<ShellView>,
) -> gpui::Stateful<gpui::Div> {
    let paper = token_rgb("--layer-background", "#ffffff");
    let ink = token_rgb("--text-primary", "#081b3a");
    let muted = token_rgb("--text-disabled", "#a6afbd");
    let red = token_rgb("--R60", "#c31818");
    let hover = token_rgb("--layer-background-hover", "#f1f2f4");
    let sep = crate::color::border_vanish();
    let is_sub = base > 0;
    div()
        .id(id)
        .absolute()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(ITEM_GAP))
        .py(px(PANEL_PAD))
        .px(px(PANEL_PAD - 2.0))
        .rounded(px(8.))
        .bg(rgb(paper))
        .border_1()
        .border_color(crate::color::border_subtle())
        .shadow_md()
        .occlude()
        .text_size(px(14.))
        .line_height(px(21.))
        .text_color(rgb(ink))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when_some(header, |el, name| {
            el.child(
                div()
                    .px(px(8.))
                    .h(px(27.))
                    .mb(px(4.))
                    .text_size(px(18.))
                    .line_height(px(27.))
                    .font_family(crate::fonts::FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .truncate()
                    .child(name),
            )
        })
        .children(rows.into_iter().enumerate().map(move |(i, row)| match row {
            Row::Sep => div()
                .id(("menu-sep", (base + i) as u64))
                .flex_none()
                .h(px(1.))
                .mx(px(4.))
                .bg(sep)
                .into_any_element(),
            Row::Item { label, icon, danger, enabled, checked, dot, action, sub } => {
                let color = if !enabled { muted } else if danger { red } else { ink };
                let has_sub = !sub.is_empty();
                let el = div()
                    .id(("menu-item", (base + i) as u64))
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .h(px(ITEM_H))
                    .px(px(10.))
                    .gap(px(12.))
                    .rounded(px(4.))
                    .text_color(rgb(color))
                    .when(enabled, |el| el.cursor_pointer().hover(|st| st.bg(rgb(hover))))
                    .when(!is_sub, |el| {
                        el.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                if let Some(menu) = this.menu.as_mut() {
                                    let want = has_sub.then_some(i);
                                    if menu.sub != want {
                                        menu.sub = want;
                                        cx.notify();
                                    }
                                }
                            }
                        }))
                    })
                    .when_some(icon, |el, code| el.child(crate::fonts::icon_el(code, "•", 20.)))
                    .when_some(dot, |el, color| {
                        el.child(div().flex_none().size(px(12.)).rounded(px(3.)).bg(rgb(color)))
                    })
                    .child(div().flex_1().min_w(px(0.)).truncate().child(crate::i18n::t(&label)))
                    .when(
                        matches!(&action, MenuAction::OpenUrl(url) if url.contains("business.zbox.vn")),
                        |el| el.child(crate::fonts::icon_el(crate::fonts::ic::EXTEND, "↗", 16.)),
                    )
                    .when(checked, |el| {
                        el.child(div().text_color(rgb(token_rgb("--B60", "#005ae0"))).child(
                            crate::fonts::icon_el(mi::CHECK, "✓", 20.),
                        ))
                    })
                    .when(has_sub, |el| el.child(crate::fonts::icon_el(mi::ARROW, "›", 20.)));
                if has_sub {
                    el.into_any_element()
                } else {
                    el.on_click(cx.listener(move |this, _, window, cx| {
                        this.run_action(action.clone(), window, cx);
                        this.close_menu();
                        cx.notify();
                    }))
                    .into_any_element()
                }
            }
        }))
}

/// Labels for tests — conversation menu, unpinned / unmuted / not archived.
#[cfg(test)]
pub fn conv_menu_labels() -> Vec<String> {
    labels_of(conv_rows(1, false, false, false, false, Vec::new()))
}

fn resolve_msg_owner(conv: &data::ChatConv, msg: &ChatMsg, self_uid: Option<&str>) -> Option<String> {
    let me = self_uid.map(str::trim).filter(|s| !s.is_empty());
    if msg.from_me || msg.sender_uid == "0" {
        me.map(str::to_owned)
    } else if !msg.sender_uid.is_empty() {
        Some(msg.sender_uid.clone())
    } else if !conv.is_group && !conv.thread_id.is_empty() {
        Some(conv.thread_id.clone())
    } else {
        None
    }
}

fn build_delete_item(
    conv: &data::ChatConv,
    msg: &ChatMsg,
    self_uid: Option<&str>,
) -> Option<zalo_client::DeleteItem> {
    let global_msg_id = msg.global_msg_id.filter(|&id| id > 0)?;
    let cli_msg_id = msg.cli_msg_id.filter(|&id| id > 0)?;
    let owner_id = resolve_msg_owner(conv, msg, self_uid)?;
    let me = self_uid.map(str::trim).filter(|s| !s.is_empty());
    let from_self = msg.from_me || msg.sender_uid == "0";
    let dest_id = if conv.is_group {
        Some(conv.thread_id.trim_start_matches("g_").to_owned()).filter(|s| !s.is_empty())
    } else if from_self {
        Some(conv.thread_id.clone()).filter(|s| !s.is_empty())
    } else {
        // Zalo PC `syncDeleteMessage`: incoming 1-1 messages set destId = me.
        me.map(str::to_owned)
    };
    Some(zalo_client::DeleteItem {
        cli_msg_id,
        global_msg_id,
        owner_id,
        dest_id,
    })
}

fn build_thread_msg_ref(
    conv: &data::ChatConv,
    msgs: &[ChatMsg],
    self_uid: Option<&str>,
) -> Option<zalo_client::ThreadMsgRef> {
    let last = msgs.iter().rev().find(|m| {
        m.cli_msg_id.is_some_and(|c| c > 0)
            && m.global_msg_id.is_some_and(|g| g > 0)
            && resolve_msg_owner(conv, m, self_uid).is_some()
    })?;
    let from_uid = resolve_msg_owner(conv, last, self_uid)?;
    Some(zalo_client::ThreadMsgRef {
        thread_id: conv.thread_id.clone(),
        is_group: conv.is_group,
        cli_msg_id: last.cli_msg_id?.to_string(),
        global_msg_id: last.global_msg_id?.to_string(),
        from_uid,
        ts: last.at_ms,
    })
}

#[cfg(test)]
fn labels_of(rows: Vec<Row>) -> Vec<String> {
    rows.into_iter()
        .filter_map(|row| match row {
            Row::Item { label, .. } => Some(label),
            Row::Sep => None,
        })
        .collect()
}

#[cfg(test)]
fn test_msg(kind: crate::media::MsgKind, text: &str) -> ChatMsg {
    let mut msg = ChatMsg::text_msg(
        1,
        1,
        "preview:1".into(),
        true,
        "Bạn".into(),
        String::new(),
        text.into(),
        "00:00".into(),
        0,
        None,
        None,
    );
    msg.kind = kind;
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MsgKind;

    #[test]
    fn conv_menu_matches_zalo_pc() {
        assert_eq!(
            conv_menu_labels(),
            [
                "Mở cửa sổ riêng",
                "Ghim hội thoại",
                "Chuyển sang mục Khác",
                "Phân loại",
                "Đánh dấu chưa đọc",
                "Tắt thông báo",
                "Ẩn trò chuyện",
                "Xóa hội thoại",
                "Báo xấu",
            ]
        );
        let rows = conv_rows(1, false, false, false, false, Vec::new());
        let mute = rows.iter().find_map(|r| match r {
            Row::Item { label, sub, .. } if label == "Tắt thông báo" => {
                Some(labels_of(sub.clone()))
            }
            _ => None,
        });
        assert_eq!(
            mute.unwrap(),
            [
                "Trong 1 giờ",
                "Trong 4 giờ",
                "Cho đến 8:00 AM",
                "Cho đến khi được mở lại"
            ]
        );
        assert!(matches!(
            &rows[rows.len() - 3],
            Row::Item { danger: true, .. }
        ));
    }

    #[test]
    fn photo_menu_matches_zalo_pc() {
        let labels = labels_of(message_rows(
            &test_msg(MsgKind::Photo, "[Hình ảnh]"),
            false,
            false,
            false,
            None,
        ));
        assert_eq!(
            labels,
            [
                "Trả lời",
                "Chia sẻ",
                "Thả cảm xúc",
                "Copy hình ảnh",
                "Lưu về máy",
                "Ghim tin nhắn",
                "Đánh dấu tin nhắn",
                "Chọn nhiều tin nhắn",
                "Xem chi tiết",
                "Tuỳ chọn khác",
                "Thu hồi",
                "Xóa chỉ ở phía tôi",
            ]
        );
    }

    #[test]
    fn cloud_text_and_call_menus() {
        let rows = message_rows(
            &test_msg(MsgKind::Text, "https://github.com/x"),
            true,
            false,
            false,
            Some(5),
        );
        let labels = labels_of(rows.clone());
        assert_eq!(labels[3], "Copy link");
        assert!(!labels.iter().any(|l| l == "Ghim tin nhắn"));
        let react_sub = rows
            .iter()
            .find_map(|r| match r {
                Row::Item { label, sub, .. } if label == "Thả cảm xúc" => {
                    Some(labels_of(sub.clone()))
                }
                _ => None,
            })
            .expect("react submenu");
        assert!(react_sub.iter().any(|l| l == "Bỏ cảm xúc"));
        let call = labels_of(message_rows(
            &test_msg(MsgKind::Call, "[Cuộc gọi]"),
            false,
            false,
            false,
            None,
        ));
        assert_eq!(
            call,
            ["Chọn nhiều tin nhắn", "Xem chi tiết", "Xóa chỉ ở phía tôi"]
        );
    }

    #[test]
    fn mute_until_8am_is_within_a_day() {
        let s = secs_until_8am(data::now_ms());
        assert!(s > 0 && s <= 86_400);
        // 07:00 VN (00:00 UTC) → one hour.
        assert_eq!(secs_until_8am(0), 3600);
    }

    #[test]
    fn menu_flips_like_zalo_pc() {
        // Room below/right: open at the cursor.
        assert_eq!(place(100.0, 200.0, 700.0), 100.0);
        // Near the bottom: open above the cursor.
        assert_eq!(place(600.0, 390.0, 700.0), 210.0);
        // Neither fits: clamp inside.
        assert_eq!(place(200.0, 690.0, 700.0), 6.0);
    }

    #[test]
    fn panel_metrics_match_live_dom() {
        // Conv menu: 9 items + 4 separators → 9·36 + 4·1 + 12·4 = 376.
        assert_eq!(
            rows_height(&conv_rows(1, false, false, false, false, Vec::new())),
            376.0
        );
    }

    #[test]
    fn contact_menu_has_chat_and_block() {
        let labels = labels_of(contact_rows("Lan Anh", false, ""));
        assert_eq!(labels[0], "Nhắn tin");
        assert!(labels.iter().any(|s| s == "Chặn tin nhắn và cuộc gọi"));
    }

    #[test]
    fn account_menu_matches_avatar_popover() {
        let labels = labels_of(account_rows());
        assert_eq!(
            labels,
            [
                str_vi("STR_UPGRADE_ACCOUNT", "Nâng cấp tài khoản"),
                str_vi("STR_ACCOUNT_MENU", "Hồ sơ của bạn"),
                str_vi("STR_MENU_SETTING", "Cài đặt"),
                str_vi("STR_MENU_LOGOUT", "Đăng xuất"),
            ]
        );
        assert!(labels
            .iter()
            .all(|s| s != "Đăng nhập lại" && s != "Thoát Zalo"));
        assert!(matches!(
            account_rows()
                .into_iter()
                .find(|r| matches!(r, Row::Item { action: MenuAction::OpenUrl(url), .. } if url.contains("zbox"))),
            Some(Row::Item { action: MenuAction::OpenUrl(_), .. })
        ));
        assert!(matches!(
            account_rows().into_iter().find(|r| matches!(
                r,
                Row::Item {
                    action: MenuAction::Logout,
                    ..
                }
            )),
            Some(Row::Item {
                action: MenuAction::Logout,
                ..
            })
        ));
    }

    #[test]
    fn gear_menu_lists_account_settings_and_logout() {
        let rows = gear_rows(true, false);
        let labels = labels_of(rows.clone());
        assert_eq!(labels[0], str_vi("STR_ACCOUNT", "Thông tin tài khoản"));
        assert_eq!(labels[1], str_vi("STR_MENU_SETTING", "Cài đặt"));
        assert!(labels.iter().any(|s| s == str_vi("STR_DATA", "Dữ liệu")));
        assert!(labels
            .iter()
            .any(|s| s == str_vi("STR_MENU_HOTKEYS", "Phím tắt")));
        assert_eq!(
            labels.last().map(String::as_str),
            Some(str_vi("STR_MENU_LOGOUT", "Đăng xuất"))
        );
        let data = rows.iter().find_map(|r| match r {
            Row::Item { label, sub, .. } if label == str_vi("STR_DATA", "Dữ liệu") => {
                Some(labels_of(sub.clone()))
            }
            _ => None,
        });
        let data = data.expect("data submenu");
        assert_eq!(
            data[0],
            str_vi("STR_SYNC_AND_BACKUP_CONFIGURATION", "Đồng bộ và sao lưu")
        );
        assert_eq!(data[1], str_vi("STR_RESOURCE_MANAGE", "Quản lý dữ liệu"));
        let logged_out = labels_of(gear_rows(false, true));
        assert_eq!(logged_out[0], str_vi("STR_MENU_ABOUT_ZALO", "Giới thiệu"));
        assert_eq!(
            logged_out.last().map(String::as_str),
            Some(str_vi("STR_MENU_QUIT", "Thoát"))
        );
        assert!(matches!(
            gear_rows(true, false)
                .into_iter()
                .find(|r| matches!(r, Row::Item { action: MenuAction::OpenSettingsPage(p), .. } if *p == crate::settings::SettingsPage::General)),
            Some(_)
        ));
    }

    #[test]
    fn screenshot_menu_checks_the_saved_mode() {
        let rows = screenshot_rows(true, "CTRL+ALT+s", "CTRL+ALT+SHIFT+s");
        match &rows[0] {
            Row::Item {
                checked,
                action,
                label,
                ..
            } => {
                assert!(*checked);
                assert!(label.contains("Ctrl + Alt + S"));
                assert!(matches!(action, MenuAction::ShotWithZalo(true)));
            }
            Row::Sep => panic!("expected a screenshot row"),
        }
        match &rows[1] {
            Row::Item {
                checked,
                action,
                label,
                ..
            } => {
                assert!(!*checked);
                assert!(label.contains("Ctrl + Alt + Shift + S"));
                assert!(matches!(action, MenuAction::ShotWithZalo(false)));
            }
            Row::Sep => panic!("expected a screenshot row"),
        }
        let theme = labels_of(theme_rows(false, true));
        assert_eq!(theme, ["Sáng", "Tối", "Hệ Thống"]);
    }

    #[test]
    fn delete_item_strips_group_prefix_and_maps_dest_like_zalo_pc() {
        let group_conv = data::ChatConv {
            id: 1,
            thread_id: "g_200000000000000002".into(),
            name: "Group".into(),
            preview: String::new(),
            time: String::new(),
            unread: 0,
            is_group: true,
            online: false,
            live: true,
        };
        let mut peer_group_msg = test_msg(MsgKind::Photo, "[Hình ảnh]");
        peer_group_msg.from_me = false;
        peer_group_msg.sender_uid = "999".into();
        peer_group_msg.cli_msg_id = Some(111);
        peer_group_msg.global_msg_id = Some(222);
        let item = build_delete_item(&group_conv, &peer_group_msg, Some("100")).unwrap();
        assert_eq!(item.owner_id, "999");
        assert_eq!(item.dest_id.as_deref(), Some("200000000000000002"));
        assert_eq!(item.cli_msg_id, 111);
        assert_eq!(item.global_msg_id, 222);

        let mut direct_conv = group_conv;
        direct_conv.thread_id = "555".into();
        direct_conv.is_group = false;

        let mut my_direct_msg = peer_group_msg.clone();
        my_direct_msg.from_me = true;
        my_direct_msg.sender_uid.clear();
        let item_out = build_delete_item(&direct_conv, &my_direct_msg, Some("100")).unwrap();
        assert_eq!(item_out.owner_id, "100");
        assert_eq!(item_out.dest_id.as_deref(), Some("555"));

        let item_in = build_delete_item(&direct_conv, &peer_group_msg, Some("100")).unwrap();
        assert_eq!(item_in.owner_id, "999");
        assert_eq!(item_in.dest_id.as_deref(), Some("100"));

        peer_group_msg.global_msg_id = None;
        assert!(build_delete_item(&direct_conv, &peer_group_msg, Some("100")).is_none());
    }

    #[test]
    fn thread_msg_ref_skips_unsynced_tail_and_uses_numeric_self_uid() {
        let conv = data::ChatConv {
            id: 1,
            thread_id: "g_264".into(),
            name: "Group".into(),
            preview: String::new(),
            time: String::new(),
            unread: 0,
            is_group: true,
            online: false,
            live: true,
        };
        let mut valid = test_msg(MsgKind::Text, "ok");
        valid.from_me = true;
        valid.sender_uid = "dkey_ignored".into();
        valid.cli_msg_id = Some(1001);
        valid.global_msg_id = Some(2002);
        valid.at_ms = 555;

        let mut unsynced = valid.clone();
        unsynced.global_msg_id = None;
        unsynced.cli_msg_id = Some(3003);

        // Unsynced tail is skipped; real numeric `self_uid` is sent as `from_uid`.
        let r = build_thread_msg_ref(&conv, &[valid, unsynced.clone()], Some("987654")).unwrap();
        assert_eq!(r.cli_msg_id, "1001");
        assert_eq!(r.global_msg_id, "2002");
        assert_eq!(r.from_uid, "987654");

        // No synced message at all -> None (never sends "0"/"0"/"").
        assert!(build_thread_msg_ref(&conv, &[unsynced], Some("987654")).is_none());
        assert!(build_thread_msg_ref(&conv, &[], Some("987654")).is_none());
    }
}
