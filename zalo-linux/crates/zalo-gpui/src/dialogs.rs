//! "Add Friend" and "Create Group" / "Add Members" dialogs, measured on the
//! live Zalo PC 26.10.10 (CDP, 12/11/2026):
//! - Add Friend: 400 × 625, header 16/500, (+84) + phone field, "People you may
//!   know" list with "Add Friend", footer "Cancel" / "Search".
//! - Create Group: 522 × 740, group name field, search field, recent chats then
//!   A–Z sections with round checkboxes, "Selected n/100", "Cancel" / "Create Group".
//!
//! Calls: `find_user_by_phone`, `get_recommended_friends`,
//! `send_friend_request`, `create_group`, `invite_group_members`.

use std::collections::HashSet;

use gpui::{
    div, img, prelude::*, px, rgb, ClickEvent, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};
use serde_json::Value;

use crate::color::token_rgb;
use crate::fonts::{self, FAMILY_SEMIBOLD};
use crate::line_input::{LineEvent, LineInput};
use crate::shell::ShellView;

/// One person row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    /// Zalo uid.
    pub uid: String,
    /// Display name.
    pub name: String,
    /// https avatar.
    pub avatar: String,
    /// Second line ("From friend suggestions", phone…).
    pub note: String,
}

/// Open dialog.
pub enum Dialog {
    /// "Add Friend".
    AddFriend(AddFriend),
    /// "Create Group" or "Add Members" (`add_to` set).
    Group(GroupPicker),
    /// Message "View Details".
    Details(crate::data::ChatMsg),
    /// Message "Share".
    Share(SharePicker),
    /// Info panel "Rename Group".
    Rename(RenameGroup),
    /// Hidden-chat PIN / screen-lock code.
    Pin(crate::privacy::PinDialog),
    /// Settings › Auto-Reply.
    AutoReply(crate::autoreply::AutoReplyDlg),
    /// "Manage Quick Messages".
    QuickMessages(crate::quick::QuickMessagesDlg),
    /// "Manage Labels".
    Labels(crate::labels::LabelsDlg),
    /// "Create Call" — pick group members before an outgoing group call.
    GroupCall(GroupCallPicker),
    /// Info panel "Group Management".
    GroupManage(crate::account::GroupManageDlg),
    /// Conversation auto-delete TTL picker (`/api/conv/autodelete/updateConvers`).
    AutoDelete(crate::account::AutoDeleteDlg),
    /// Account menu "Your Profile".
    Profile(crate::account::ProfileDlg),
    /// Clicking a `@mention` or peer opens their Zalo account info card.
    UserInfo(crate::account::UserInfoDlg),
    /// Settings "Decrypt .zdb4 Backup".
    BackupDecrypt(crate::account::BackupDecryptDlg),
    /// Composer "Send Contact Card".
    Namecard(NamecardPicker),
    /// Composer "Send Bank Account".
    BankCard(BankCardDlg),
    /// Group / chat board: Reminder, Note, Pin, Poll.
    Board(crate::board::BoardDlg),
    /// Excel paste format picker dialog ("As Text" / "As Image").
    PasteChoice(PasteChoiceDlg),
    /// Set alias for friend.
    Alias(AliasDlg),
    /// Report user / group.
    Report(ReportDlg),
    /// Broadcast message to multiple conversations.
    Broadcast(BroadcastDlg),
    /// Confirmation dialog to remove a friend.
    RemoveFriend(RemoveFriendDlg),
    /// Toggle 2FA security via OTP (`/api/update/setting/2FA`).
    TwoFactorOtp(TwoFactorOtpDlg),
    /// Guide on finding a newly hidden conversation ("Conversation hidden successfully").
    HiddenGuide(String),
    /// "About / Version info" dialog (from Settings › Help › Version info or titlebar badge).
    About,
    /// In-app auto-update notification & one-click installer popup.
    UpdateAvailable,
    /// "Contact / Support" dialog (from Settings › Help › Contact).
    Contact,
}

/// Paste Excel table / clipboard content containing both image and text.
pub struct PasteChoiceDlg {
    pub image: gpui::Image,
    pub text: String,
}

/// Set alias for friend (`set_alias`).
pub struct AliasDlg {
    pub uid: String,
    pub name: String,
    pub input: Entity<LineInput>,
    pub busy: bool,
}

/// Report conversation / user (`report_abuse`).
pub struct ReportDlg {
    pub id: String,
    pub name: String,
    pub is_group: bool,
    pub reason: usize,
    pub input: Entity<LineInput>,
}

/// Tools › Broadcast Message.
pub struct BroadcastDlg {
    pub query: Entity<LineInput>,
    pub query_text: String,
    pub picked: Vec<u64>,
    pub message: Entity<LineInput>,
}

/// Confirmation dialog to remove a friend.
pub struct RemoveFriendDlg {
    pub uid: String,
    pub name: String,
}

/// Toggle 2FA security with OTP code (`/api/update/setting/2FA`).
pub struct TwoFactorOtpDlg {
    pub turn_on: bool,
    pub otp: Entity<LineInput>,
    pub otp_text: String,
    pub busy: bool,
    pub status: Option<String>,
    pub error: Option<String>,
}

/// "Send Contact Card" friend/contact picker.
pub struct NamecardPicker {
    query: Entity<LineInput>,
    query_text: String,
}

/// "Send Bank Account" transfer card form.
pub struct BankCardDlg {
    bank: Entity<LineInput>,
    bank_text: String,
    account: Entity<LineInput>,
    acc_text: String,
    owner: Entity<LineInput>,
    amount: Entity<LineInput>,
    note: Entity<LineInput>,
}

/// Outgoing group call: who to ring.
pub struct GroupCallPicker {
    conv_id: u64,
    video: bool,
    query: Entity<LineInput>,
    query_text: String,
    picked: Vec<String>,
    /// `maxUsers - 1`. The caller is already in the room.
    max_invites: usize,
    error: Option<String>,
}

/// "Rename Group" state.
pub struct RenameGroup {
    conv: u64,
    input: Entity<LineInput>,
    busy: bool,
    error: Option<String>,
}

/// "Share" state: the message, search, picked conversations, extra note.
pub struct SharePicker {
    msgs: Vec<crate::data::ChatMsg>,
    query: Entity<LineInput>,
    query_text: String,
    picked: Vec<u64>,
    note: Entity<LineInput>,
}

/// "Add Friend" state.
pub struct AddFriend {
    phone: Entity<LineInput>,
    found: Option<Result<Person, String>>,
    suggestions: Vec<Person>,
    sent: HashSet<String>,
    busy: bool,
}

/// Group picker state.
pub struct GroupPicker {
    add_to: Option<(String, String)>,
    name: Entity<LineInput>,
    query: Entity<LineInput>,
    query_text: String,
    label_filter: Option<i64>,
    label_scroll: gpui::ScrollHandle,
    picked: Vec<String>,
    busy: bool,
    error: Option<String>,
}

fn json_str(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `find_user_by_phone` → person (zalo-gui `found_user_from`).
pub(crate) fn found_user_from(value: &Value) -> Option<Person> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let uid = json_str(root, "uid").filter(|s| !s.is_empty() && s != "0")?;
    let name = ["display_name", "zalo_name", "displayName", "zaloName"]
        .iter()
        .find_map(|k| json_str(root, k).filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| uid.clone());
    let avatar = json_str(root, "avatar")
        .filter(|u| u.starts_with("https://"))
        .unwrap_or_default();
    Some(Person {
        uid,
        name,
        avatar,
        note: String::new(),
    })
}

/// `get_recommended_friends` → `recommItems[].dataInfo`.
fn recomms_from(value: &Value) -> Vec<Person> {
    let root = value.get("data").unwrap_or(value);
    root.get("recommItems")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let info = item.get("dataInfo")?;
                    let uid = json_str(info, "userId").filter(|s| !s.is_empty())?;
                    let name = json_str(info, "displayName")
                        .filter(|s| !s.is_empty())
                        .or_else(|| json_str(info, "zaloName"))
                        .unwrap_or_else(|| uid.clone());
                    let avatar = json_str(info, "avatar").unwrap_or_default();
                    let note = info
                        .get("recommInfo")
                        .and_then(|r| json_str(r, "message"))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Từ gợi ý kết bạn".into());
                    Some(Person {
                        uid,
                        name,
                        avatar,
                        note,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl ShellView {
    /// Search bar "Add Friend".
    pub(crate) fn open_add_friend(&mut self, cx: &mut Context<Self>) {
        let phone = cx.new(|cx| LineInput::new(cx, "Số điện thoại"));
        cx.subscribe(&phone, |this, input, event: &LineEvent, cx| {
            if let LineEvent::Submit(_) = event {
                let text = input.read(cx).text();
                this.find_phone(text, cx);
            }
        })
        .detach();
        self.dialog = Some(Dialog::AddFriend(AddFriend {
            phone,
            found: None,
            suggestions: Vec::new(),
            sent: HashSet::new(),
            busy: false,
        }));
        self.load_suggestions(cx);
    }

    /// Search bar "Create Group" (`add_to` None) or header "Add Members".
    pub(crate) fn open_group_picker(
        &mut self,
        add_to: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let name = cx.new(|cx| LineInput::new(cx, "Nhập tên nhóm..."));
        let query = cx
            .new(|cx| LineInput::new(cx, "Nhập tên, số điện thoại, hoặc danh sách số điện thoại"));
        cx.subscribe(&query, |this, _, event: &LineEvent, cx| {
            if let LineEvent::Change(text) = event {
                if let Some(Dialog::Group(g)) = this.dialog.as_mut() {
                    g.query_text = text.clone();
                    cx.notify();
                }
            }
        })
        .detach();
        // Member list predates the cache field: fetch profiles once more.
        if let Some((grid, _)) = add_to.as_ref() {
            let known = self
                .profiles
                .map
                .get(&format!("g_{grid}"))
                .is_some_and(|p| !p.member_ids.is_empty());
            if !known && self.has_session {
                self.profiles_refresh = true;
            }
        }
        self.dialog = Some(Dialog::Group(GroupPicker {
            add_to,
            name,
            query,
            query_text: String::new(),
            label_filter: None,
            label_scroll: gpui::ScrollHandle::new(),
            picked: Vec::new(),
            busy: false,
            error: None,
        }));
        self.ensure_labels();
        self.avatar_all = true;
        self.avatars_queued = true;
    }

    /// Header ☎ / video on a group: "Create Call", then ring only the ticked people.
    pub(crate) fn open_group_call(&mut self, conv_id: u64, video: bool, cx: &mut Context<Self>) {
        let Some(conv) = self.conv(conv_id).cloned() else {
            return;
        };
        let profile = self.profiles.map.get(&conv.thread_id);
        let member_ids = profile.map(|p| p.member_ids.clone()).unwrap_or_default();
        let mine = self.self_member_ids();
        let others = member_ids
            .iter()
            .filter(|id| !id.is_empty() && !mine.contains(*id))
            .count();
        if others == 0 {
            self.set_note("Chưa có danh sách thành viên để gọi nhóm");
            return;
        }
        let head = profile
            .map(|p| p.members)
            .unwrap_or(0)
            .max(member_ids.len() as u32)
            .max(2);
        let max_invites = (head as usize).saturating_sub(1).max(1);
        let query = cx.new(|cx| LineInput::new(cx, "Tìm kiếm bạn bè trong nhóm"));
        cx.subscribe(&query, |this, _, event: &LineEvent, cx| {
            if let LineEvent::Change(text) = event {
                if let Some(Dialog::GroupCall(picker)) = this.dialog.as_mut() {
                    picker.query_text = text.clone();
                    cx.notify();
                }
            }
        })
        .detach();
        self.dialog = Some(Dialog::GroupCall(GroupCallPicker {
            conv_id,
            video,
            query,
            query_text: String::new(),
            picked: Vec::new(),
            max_invites,
            error: None,
        }));
        self.avatar_all = true;
        self.avatars_queued = true;
    }

    fn submit_group_call(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::GroupCall(picker)) = self.dialog.as_ref() else {
            return;
        };
        if picker.picked.is_empty() {
            return;
        }
        let conv_id = picker.conv_id;
        let video = picker.video;
        let picked = picker.picked.clone();
        self.dialog = None;
        self.launch_group_call(conv_id, video, &picked, cx);
        cx.notify();
    }

    fn load_suggestions(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let v = api
                            .get_recommended_friends()
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(recomms_from(&v))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let (Some(Dialog::AddFriend(d)), Ok(list)) = (view.dialog.as_mut(), result) {
                    d.suggestions = list;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn find_phone(&mut self, raw: String, cx: &mut Context<Self>) {
        let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
        let phone = match digits.strip_prefix('0') {
            Some(rest) => format!("84{rest}"),
            None if digits.starts_with("84") => digits,
            None => format!("84{digits}"),
        };
        let Some(Dialog::AddFriend(d)) = self.dialog.as_mut() else {
            return;
        };
        if phone.len() < 10 {
            d.found = Some(Err("Số điện thoại không hợp lệ".into()));
            return;
        }
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            d.found = Some(Err("Chưa đăng nhập".into()));
            return;
        };
        d.busy = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let v = api
                            .find_user_by_phone(&phone)
                            .await
                            .map_err(|e| e.to_string())?;
                        found_user_from(&v)
                            .ok_or_else(|| "Không tìm thấy tài khoản Zalo với số này.".into())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Ok(ref person) = result {
                    view.recent_phone_searches
                        .retain(|p| p.uid != person.uid);
                    view.recent_phone_searches.insert(0, person.clone());
                    view.recent_phone_searches.truncate(5);
                }
                if let Some(Dialog::AddFriend(d)) = view.dialog.as_mut() {
                    d.busy = false;
                    d.found = Some(result);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn send_request(&mut self, uid: String, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        let me = self.display_name.clone().unwrap_or_default();
        let greeting = if me.is_empty() {
            "Xin chào, kết bạn với mình nhé!".to_owned()
        } else {
            format!("Xin chào, mình là {me}. Kết bạn với mình nhé!")
        };
        self.sent_friend_reqs.insert(uid.clone());
        if let Some(Dialog::AddFriend(d)) = self.dialog.as_mut() {
            d.sent.insert(uid.clone());
        }
        let req_uid = uid.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.send_friend_request(&uid, &greeting)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(_) => view.set_note("Đã gửi lời mời kết bạn."),
                    Err(e) => {
                        view.sent_friend_reqs.remove(&req_uid);
                        view.set_note(format!("Gửi lời mời thất bại: {e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_group(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Group(g)) = self.dialog.as_mut() else {
            return;
        };
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            g.error = Some("Chưa đăng nhập".into());
            return;
        };
        let members = g.picked.clone();
        let name = g.name.read(cx).text().trim().to_owned();
        let add_to = g.add_to.clone();
        if add_to.is_none() && members.len() < 2 {
            g.error = Some("Chọn ít nhất 2 người để tạo nhóm".into());
            return;
        }
        g.busy = true;
        g.error = None;
        cx.spawn(async move |this, cx| {
            let target = add_to.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        match target {
                            Some((grid, _)) => api
                                .invite_group_members(&grid, &members)
                                .await
                                .map(|_| String::new())
                                .map_err(|e| e.to_string()),
                            None => {
                                let v = api
                                    .create_group(&name, &members)
                                    .await
                                    .map_err(|e| e.to_string())?;
                                let root = v.get("data").filter(|x| x.is_object()).unwrap_or(&v);
                                json_str(root, "groupId")
                                    .filter(|id| !id.is_empty())
                                    .ok_or_else(|| "Máy chủ không trả về mã nhóm.".into())
                            }
                        }
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(_) => {
                        view.dialog = None;
                        view.set_note(if add_to.is_some() {
                            "Đã thêm thành viên."
                        } else {
                            "Đã tạo nhóm."
                        });
                        view.profiles_stale();
                    }
                    Err(e) => {
                        if let Some(Dialog::Group(g)) = view.dialog.as_mut() {
                            g.busy = false;
                            g.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Friends known to this client (server profiles + 1-1 threads), A→Z.
    fn people(&self) -> (Vec<Person>, Vec<Person>) {
        let mut all: Vec<Person> = self
            .profiles
            .map
            .iter()
            .filter(|(k, _)| !k.starts_with("g_") && k.as_str() != crate::profiles::ME)
            .map(|(k, p)| Person {
                uid: k.clone(),
                name: p.name.clone(),
                avatar: p.avatar.clone(),
                note: String::new(),
            })
            .collect();
        for c in self.convs.iter().filter(|c| !c.is_group && c.live) {
            if !all.iter().any(|p| p.uid == c.thread_id) {
                all.push(Person {
                    uid: c.thread_id.clone(),
                    name: c.name.clone(),
                    ..Person::default()
                });
            }
        }
        let me = self.send2me_id.clone().unwrap_or_default();
        // Official accounts (the «Zalo» system thread) cannot join groups.
        all.retain(|p| p.uid != me && !p.name.is_empty() && p.name != "Zalo");
        all.sort_by_key(|p| p.name.to_lowercase());
        let recent: Vec<Person> = self
            .convs
            .iter()
            .filter(|c| !c.is_group && c.live && c.thread_id != me)
            .take(5)
            .filter_map(|c| all.iter().find(|p| p.uid == c.thread_id).cloned())
            .collect();
        (recent, all)
    }

    /// Backdrop + the open dialog.
    pub(crate) fn dialog_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.dialog.as_ref() {
            Some(Dialog::AddFriend(d)) => self.add_friend_el(d, cx).into_any_element(),
            Some(Dialog::Group(g)) => self.group_el(g, cx).into_any_element(),
            Some(Dialog::Details(m)) => self.details_el(m, cx).into_any_element(),
            Some(Dialog::Share(s)) => self.share_el(s, cx).into_any_element(),
            Some(Dialog::Rename(r)) => self.rename_el(r, cx).into_any_element(),
            Some(Dialog::Pin(d)) => self.pin_el(d, cx).into_any_element(),
            Some(Dialog::AutoReply(d)) => self.auto_reply_el(d, cx).into_any_element(),
            Some(Dialog::QuickMessages(d)) => self.quick_messages_el(d, cx).into_any_element(),
            Some(Dialog::Labels(d)) => self.labels_el(d, cx).into_any_element(),
            Some(Dialog::GroupCall(g)) => self.group_call_el(g, cx).into_any_element(),
            // Rendered as Electron-style right-sidebar slide-in panels inside `info.rs`.
            Some(Dialog::GroupManage(_)) | Some(Dialog::Board(_)) => {
                return div().into_any_element();
            }
            Some(Dialog::AutoDelete(a)) => self.auto_delete_el(a, cx).into_any_element(),
            Some(Dialog::Profile(p)) => self.profile_el(p, cx).into_any_element(),
            Some(Dialog::UserInfo(u)) => self.user_info_el(u, cx).into_any_element(),
            Some(Dialog::BackupDecrypt(b)) => self.backup_decrypt_el(b, cx).into_any_element(),
            Some(Dialog::Namecard(n)) => self.namecard_el(n, cx).into_any_element(),
            Some(Dialog::BankCard(b)) => self.bank_card_el(b, cx).into_any_element(),
            Some(Dialog::PasteChoice(p)) => self.paste_choice_el(p, cx).into_any_element(),
            Some(Dialog::Alias(a)) => self.alias_el(a, cx).into_any_element(),
            Some(Dialog::Report(r)) => self.report_el(r, cx).into_any_element(),
            Some(Dialog::Broadcast(b)) => self.broadcast_el(b, cx).into_any_element(),
            Some(Dialog::RemoveFriend(f)) => self.remove_friend_el(f, cx).into_any_element(),
            Some(Dialog::TwoFactorOtp(t)) => self.two_factor_el(t, cx).into_any_element(),
            Some(Dialog::HiddenGuide(name)) => self.hidden_guide_el(name, cx).into_any_element(),
            Some(Dialog::About) => self.about_el(cx).into_any_element(),
            Some(Dialog::UpdateAvailable) => self.update_available_el(cx).into_any_element(),
            Some(Dialog::Contact) => self.contact_el(cx).into_any_element(),
            None => return div().into_any_element(),
        };
        div()
            .id("dialog-layer")
            .absolute()
            .inset(px(0.))
            .child(
                div()
                    .id("dialog-backdrop")
                    .absolute()
                    .inset(px(0.))
                    .occlude()
                    .bg(crate::color::modal_scrim())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if matches!(this.dialog, Some(Dialog::UpdateAvailable))
                            && this.update_install.is_busy()
                        {
                            return;
                        }
                        this.dialog = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .absolute()
                    .inset(px(0.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("dialog-card")
                            .h_full()
                            .py(px(12.))
                            .flex()
                            .flex_col()
                            .justify_center()
                            .occlude()
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation()
                            })
                            .child(body),
                    ),
            )
            .into_any_element()
    }

    fn dialog_header(&self, title: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        self.dialog_title_owned(crate::i18n::t(title).to_owned(), cx)
    }

    fn dialog_title_owned(&self, title: String, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(48.))
            .px(px(16.))
            .border_b_1()
            .border_color(crate::color::border_subtle())
            .child(
                div()
                    .text_size(px(16.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .id("dialog-close")
                    .size(px(28.))
                    .rounded(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dialog = None;
                        cx.notify();
                    }))
                    .child(fonts::icon_el('\u{EA9F}', "×", 20.)),
            )
    }

    /// Title row with ×, for dialogs built in other modules.
    pub(crate) fn dialog_title_bar(&self, title: &str, cx: &mut Context<Self>) -> impl IntoElement {
        self.dialog_title_owned(title.to_owned(), cx)
    }

    /// Avatar used by dialogs and the members page.
    pub(crate) fn dialog_avatar(&self, p: &Person, size: f32) -> gpui::AnyElement {
        self.person_avatar(p, size)
    }

    fn person_avatar(&self, p: &Person, size: f32) -> gpui::AnyElement {
        let stored = self
            .profiles
            .map
            .get(&p.uid)
            .map(|profile| profile.avatar.as_str())
            .unwrap_or("");
        let url = if p.avatar.trim().is_empty() {
            stored
        } else {
            p.avatar.as_str()
        };
        if !url.contains("/default") {
            if let Some(image) = self.avatar_of(&p.uid) {
                return img(image)
                    .size(px(size))
                    .rounded(px(size / 2.0))
                    .object_fit(gpui::ObjectFit::Cover)
                    .into_any_element();
            }
        }
        crate::sidebar::letter_disc(
            &crate::sidebar::avatar_initials(&p.name),
            crate::sidebar::avatar_color(&p.uid),
            size,
        )
        .into_any_element()
    }

    fn add_friend_el(&self, d: &AddFriend, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let row = |this: &Self, p: &Person, sent: bool, cx: &mut Context<Self>| {
            let uid = p.uid.clone();
            let chat_uid = p.uid.clone();
            div()
                .id(gpui::SharedString::from(format!("friend-{}", p.uid)))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.))
                .py(px(8.))
                .child(this.person_avatar(p, 40.))
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
                                .truncate()
                                .child(p.name.clone()),
                        )
                        .when(!p.note.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(muted))
                                    .truncate()
                                    .child(p.note.clone()),
                            )
                        }),
                )
                .when(this.convs.iter().any(|c| c.thread_id == p.uid), |el| {
                    el.child(
                        div()
                            .id(gpui::SharedString::from(format!("chat-{}", p.uid)))
                            .px(px(12.))
                            .h(px(32.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.dialog = None;
                                if let Some(id) = this
                                    .convs
                                    .iter()
                                    .find(|c| c.thread_id == chat_uid)
                                    .map(|c| c.id)
                                {
                                    this.select_conv(id);
                                }
                                cx.notify();
                            }))
                            .child("Nhắn tin"),
                    )
                })
                .child(
                    div()
                        .id(gpui::SharedString::from(format!("add-{}", p.uid)))
                        .px(px(12.))
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(rgb(blue))
                        .text_size(px(14.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(if sent { muted } else { blue }))
                        .when(!sent, |el| {
                            el.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send_request(uid.clone(), cx);
                                    cx.notify();
                                }))
                        })
                        .child(if sent { "Đã gửi" } else { "Kết bạn" }),
                )
        };
        let found = d.found.clone();
        div()
            .w(px(400.))
            .h(px(625.))
            .max_h(gpui::relative(1.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_header("Thêm bạn", cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_end()
                    .gap(px(16.))
                    .px(px(16.))
                    .pt(px(16.))
                    .child(
                        div()
                            .w(px(108.))
                            .h(px(40.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(crate::color::border_subtle())
                            .text_size(px(14.))
                            .font_family(FAMILY_SEMIBOLD)
                            .child("🇻🇳 (+84)")
                            .child(
                                div()
                                    .text_color(rgb(muted))
                                    .child(fonts::icon_el(fonts::ic::SOLID_DOWN, "▾", 14.)),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(40.))
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(rgb(blue))
                            .text_size(px(14.))
                            .child(d.phone.clone()),
                    ),
            )
            .child(
                div()
                    .id("add-friend-body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(px(16.))
                    .pt(px(16.))
                    .when(d.busy, |el| {
                        el.child(div().text_color(rgb(muted)).child("Đang tìm…"))
                    })
                    .when_some(found, |el, f| match f {
                        Ok(p) => {
                            let sent = d.sent.contains(&p.uid);
                            el.child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(muted))
                                    .child("Kết quả"),
                            )
                            .child(row(self, &p, sent, cx))
                        }
                        Err(e) => el.child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(token_rgb("--R60", "#c31818")))
                                .child(e),
                        ),
                    })
                    .child(
                        div()
                            .mt(px(4.))
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child("Kết quả gần nhất"),
                    )
                    .when(self.recent_phone_searches.is_empty(), |el| {
                        el.child(
                            div()
                                .py(px(8.))
                                .text_size(px(13.))
                                .text_color(rgb(muted))
                                .child("Chưa có kết quả tìm kiếm gần đây"),
                        )
                    })
                    .children(self.recent_phone_searches.iter().map(|p| {
                        let sent = d.sent.contains(&p.uid);
                        row(self, p, sent, cx).into_any_element()
                    }))
                    .child(
                        div()
                            .mt(px(8.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child(fonts::icon_el('\u{EC91}', "☺", 14.))
                            .child("Có thể bạn quen"),
                    )
                    .children(d.suggestions.iter().map(|p| {
                        let sent = d.sent.contains(&p.uid);
                        row(self, p, sent, cx).into_any_element()
                    })),
            )
            .child(footer(
                "Hủy",
                "Tìm kiếm",
                !d.busy,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    let text = match this.dialog.as_ref() {
                        Some(Dialog::AddFriend(d)) => d.phone.read(cx).text(),
                        _ => return,
                    };
                    this.find_phone(text, cx);
                    cx.notify();
                }),
            ))
    }

    fn group_el(&self, g: &GroupPicker, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
        let hover = token_rgb("--layer-background-hover", "#e5e7eb");
        let (recent, all) = self.people();
        let q = g.query_text.trim().to_lowercase();
        let label_filter = g.label_filter;
        let matches = |p: &Person| {
            (q.is_empty() || p.name.to_lowercase().contains(&q) || p.uid.contains(&q))
                && label_filter.is_none_or(|lid| self.thread_label(&p.uid) == Some(lid))
        };
        let mut items: Vec<gpui::AnyElement> = Vec::new();
        let section = |title: String, items: &mut Vec<gpui::AnyElement>| {
            items.push(
                div()
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .text_size(px(14.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child(title)
                    .into_any_element(),
            );
        };
        let picked: HashSet<&String> = g.picked.iter().collect();
        // "Add Members": people already in the group are ticked and
        // locked, like Zalo PC ("Joined").
        let joined: HashSet<String> = g
            .add_to
            .as_ref()
            .and_then(|(grid, _)| self.profiles.map.get(&format!("g_{grid}")))
            .map(|p| p.member_ids.iter().cloned().collect())
            .unwrap_or_default();
        let push =
            |this: &Self, p: &Person, items: &mut Vec<gpui::AnyElement>, cx: &mut Context<Self>| {
                let member = joined.contains(&p.uid);
                let on = member || picked.contains(&p.uid);
                let uid = p.uid.clone();
                items.push(
                    div()
                        .id(gpui::SharedString::from(format!(
                            "pick-{}-{}",
                            p.uid,
                            items.len()
                        )))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(12.))
                        .h(px(52.))
                        .px(px(10.))
                        .rounded(px(6.))
                        .when(!member, |el| {
                            el.cursor_pointer().hover(|st| {
                                st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4")))
                            })
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if member {
                                return;
                            }
                            if let Some(Dialog::Group(g)) = this.dialog.as_mut() {
                                if let Some(i) = g.picked.iter().position(|u| *u == uid) {
                                    g.picked.remove(i);
                                } else if g.picked.len() < 100 {
                                    g.picked.push(uid.clone());
                                }
                            }
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_color(if member {
                                    crate::color::accent_disabled_bg()
                                } else if on {
                                    rgb(blue).into()
                                } else {
                                    rgb(token_rgb("--text-disabled", "#8b96a7")).into()
                                })
                                .child(fonts::icon_el(
                                    if on { '\u{ECEF}' } else { '\u{EDD3}' },
                                    "○",
                                    22.,
                                )),
                        )
                        .child(this.person_avatar(p, 40.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .text_color(rgb(ink))
                                        .truncate()
                                        .child(p.name.clone()),
                                )
                                .when(member, |el| {
                                    el.child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(muted))
                                            .child("Đã tham gia"),
                                    )
                                }),
                        )
                        .into_any_element(),
                );
            };
        if q.is_empty() && label_filter.is_none() && !recent.is_empty() {
            section("Trò chuyện gần đây".into(), &mut items);
            for p in &recent {
                push(self, p, &mut items, cx);
            }
        }
        let mut letter = String::new();
        for p in all.iter().filter(|p| matches(p)) {
            let l = p
                .name
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default();
            if l != letter {
                letter = l.clone();
                section(l, &mut items);
            }
            push(self, p, &mut items, cx);
        }
        let creating = g.add_to.is_none();
        let ok = !g.busy
            && (if creating {
                g.picked.len() >= 2
            } else {
                !g.picked.is_empty()
            });
        div()
            .w(px(522.))
            .h(px(740.))
            .max_h(gpui::relative(1.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_header(
                if creating {
                    "Tạo nhóm"
                } else {
                    "Thêm thành viên"
                },
                cx,
            ))
            .when(creating, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(14.))
                        .px(px(16.))
                        .pt(px(16.))
                        .child(
                            div()
                                .size(px(48.))
                                .rounded(px(24.))
                                .border_1()
                                .border_color(crate::color::border_subtle())
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(muted))
                                .child(fonts::icon_el('\u{EBFA}', "📷", 24.)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .h(px(37.))
                                .flex()
                                .items_center()
                                .border_b_1()
                                .border_color(crate::color::border_subtle())
                                .text_size(px(14.))
                                .child(g.name.clone()),
                        ),
                )
            })
            .when_some(g.add_to.clone(), |el, (_, name)| {
                el.child(
                    div()
                        .px(px(16.))
                        .pt(px(12.))
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .child(format!("Nhóm: {name}")),
                )
            })
            .child(
                div()
                    .mx(px(16.))
                    .mt(px(16.))
                    .h(px(36.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(18.))
                    .border_1()
                    .border_color(crate::color::border_subtle())
                    .text_size(px(14.))
                    .child(fonts::icon_el('\u{ED15}', "⌕", 14.))
                    .child(div().flex_1().child(g.query.clone())),
            )
            .child({
                let label_scroll_prev = g.label_scroll.clone();
                let label_scroll_next = g.label_scroll.clone();
                let label_scroll_wheel = g.label_scroll.clone();
                let cur_x = f32::from(g.label_scroll.offset().x);
                let max_x = f32::from(g.label_scroll.max_offset().width);
                let has_overflow = max_x > 1.0 || self.labels.len() > 3;
                let can_prev = cur_x < -1.0;
                let can_next = max_x <= 1.0 || cur_x > -(max_x - 1.0);
                let arrow_col_on = token_rgb("--icon-primary", "#081b3a");
                let arrow_col_off = token_rgb("--text-secondary", "#9aa5b4");
                div()
                    .id("group-label-bar")
                    .w_full()
                    .px(px(6.))
                    .mt(px(10.))
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .when(has_overflow, |row| {
                        row.child(
                            div()
                                .id("group-chip-prev")
                                .flex_none()
                                .size(px(24.))
                                .rounded(px(12.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(if can_prev { arrow_col_on } else { arrow_col_off }))
                                .when(can_prev, |el| {
                                    el.cursor_pointer().hover(move |st| st.bg(rgb(hover)))
                                })
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    let cur = label_scroll_prev.offset();
                                    let next_x = (cur.x + px(140.)).min(px(0.));
                                    label_scroll_prev.set_offset(gpui::point(next_x, cur.y));
                                    cx.notify();
                                }))
                                .child(fonts::icon_el(fonts::ic::CHEVRON_LEFT, "‹", 13.)),
                        )
                    })
                    .child(
                        div()
                            .id("group-label-chips")
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .overflow_x_scroll()
                            .track_scroll(&g.label_scroll)
                            .on_scroll_wheel(cx.listener(
                                move |_, e: &gpui::ScrollWheelEvent, _, cx| {
                                    let delta = e.delta.pixel_delta(px(28.));
                                    let step = if f32::from(delta.x).abs() > 0.5 {
                                        delta.x
                                    } else {
                                        delta.y
                                    };
                                    let cur = label_scroll_wheel.offset();
                                    let max = label_scroll_wheel.max_offset().width;
                                    let next_x = (cur.x + step).clamp(-max, px(0.));
                                    label_scroll_wheel.set_offset(gpui::point(next_x, cur.y));
                                    cx.notify();
                                },
                            ))
                            .child(
                                div()
                                    .id("group-chip-all")
                                    .flex_none()
                                    .h(px(26.))
                                    .px(px(12.))
                                    .rounded(px(13.))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .bg(rgb(if label_filter.is_none() { blue } else { field }))
                                    .text_color(rgb(if label_filter.is_none() {
                                        crate::color::on_accent_fg()
                                    } else {
                                        ink
                                    }))
                                    .when(label_filter.is_some(), |el| {
                                        el.hover(move |st| st.bg(rgb(hover)))
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(Dialog::Group(g)) = this.dialog.as_mut() {
                                            g.label_filter = None;
                                        }
                                        cx.notify();
                                    }))
                                    .child("Tất cả"),
                            )
                            .children(self.labels.iter().map(|label| {
                                let lid = label.id;
                                let on = label_filter == Some(lid);
                                let dot = crate::color::parse_hex(&label.color);
                                div()
                                    .id(("group-chip", lid as u64))
                                    .flex_none()
                                    .h(px(26.))
                                    .px(px(10.))
                                    .rounded(px(13.))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.))
                                    .cursor_pointer()
                                    .text_size(px(13.))
                                    .bg(rgb(if on { blue } else { field }))
                                    .text_color(rgb(if on {
                                        crate::color::on_accent_fg()
                                    } else {
                                        ink
                                    }))
                                    .when(!on, |el| el.hover(move |st| st.bg(rgb(hover))))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(Dialog::Group(g)) = this.dialog.as_mut() {
                                            g.label_filter = if g.label_filter == Some(lid) {
                                                None
                                            } else {
                                                Some(lid)
                                            };
                                        }
                                        cx.notify();
                                    }))
                                    .when_some(dot, |el, c| {
                                        el.child(
                                            div()
                                                .size(px(8.))
                                                .rounded(px(4.))
                                                .bg(rgb(if on {
                                                    crate::color::on_accent_fg()
                                                } else {
                                                    c
                                                })),
                                        )
                                    })
                                    .child(label.text.clone())
                            })),
                    )
                    .when(has_overflow, |row| {
                        row.child(
                            div()
                                .id("group-chip-next")
                                .flex_none()
                                .size(px(24.))
                                .rounded(px(12.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(if can_next { arrow_col_on } else { arrow_col_off }))
                                .when(can_next, |el| {
                                    el.cursor_pointer().hover(move |st| st.bg(rgb(hover)))
                                })
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    let cur = label_scroll_next.offset();
                                    let max = label_scroll_next.max_offset().width;
                                    let next_x = (cur.x - px(140.)).max(-max);
                                    label_scroll_next.set_offset(gpui::point(next_x, cur.y));
                                    cx.notify();
                                }))
                                .child(fonts::icon_el(fonts::ic::CHEVRON_RIGHT, "›", 13.)),
                        )
                    })
            })
            .child(
                div()
                    .mx(px(16.))
                    .mt(px(12.))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .text_size(px(13.))
                    .text_color(rgb(muted))
                    .child(format!("{} người", all.len()))
                    .child(
                        div()
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(if g.picked.is_empty() { muted } else { blue }))
                            .child(format!("Đã chọn {}/100", g.picked.len())),
                    ),
            )
            .child(
                div()
                    .id("group-picker-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .mx(px(16.))
                    .mt(px(8.))
                    .border_t_1()
                    .border_color(crate::color::border_subtle())
                    .children(items),
            )
            .when_some(g.error.clone(), |el, e| {
                el.child(
                    div()
                        .px(px(16.))
                        .py(px(6.))
                        .text_size(px(13.))
                        .text_color(rgb(token_rgb("--R60", "#c31818")))
                        .child(e),
                )
            })
            .child(footer(
                "Hủy",
                if creating {
                    "Tạo nhóm"
                } else {
                    "Xác nhận"
                },
                ok,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.submit_group(cx);
                    cx.notify();
                }),
            ))
    }

    fn group_call_el(&self, g: &GroupCallPicker, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let Some(conv) = self.conv(g.conv_id).cloned() else {
            return div().into_any_element();
        };
        let mine = self.self_member_ids();
        let q = g.query_text.trim().to_lowercase();
        let people: Vec<Person> = self
            .profiles
            .map
            .get(&conv.thread_id)
            .map(|profile| profile.member_ids.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|id| !id.is_empty() && !mine.contains(id))
            .map(|uid| {
                let profile = self.profiles.map.get(&uid);
                let name = profile
                    .map(|person| person.name.trim().to_owned())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "Thành viên".to_owned());
                Person {
                    uid,
                    name,
                    avatar: profile
                        .map(|person| person.avatar.clone())
                        .unwrap_or_default(),
                    note: String::new(),
                }
            })
            .filter(|person| q.is_empty() || person.name.to_lowercase().contains(&q))
            .collect();
        let picked: HashSet<&String> = g.picked.iter().collect();
        let max_invites = g.max_invites;
        let mut items: Vec<gpui::AnyElement> = Vec::new();
        for person in &people {
            let on = picked.contains(&person.uid);
            let uid = person.uid.clone();
            items.push(
                div()
                    .id(gpui::SharedString::from(format!("gcall-{}", person.uid)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .h(px(52.))
                    .px(px(10.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|st| st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4"))))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(Dialog::GroupCall(picker)) = this.dialog.as_mut() {
                            if let Some(i) = picker.picked.iter().position(|id| *id == uid) {
                                picker.picked.remove(i);
                                picker.error = None;
                            } else if picker.picked.len() >= picker.max_invites {
                                picker.error = Some(format!(
                                    "Bạn chỉ có thể mời {} người khác",
                                    picker.max_invites
                                ));
                            } else {
                                picker.picked.push(uid.clone());
                                picker.error = None;
                            }
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_color(rgb(if on {
                                blue
                            } else {
                                token_rgb("--text-disabled", "#8b96a7")
                            }))
                            .child(fonts::icon_el(
                                if on { '\u{ECEF}' } else { '\u{EDD3}' },
                                "○",
                                22.,
                            )),
                    )
                    .child(self.person_avatar(person, 40.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(rgb(ink))
                            .truncate()
                            .child(person.name.clone()),
                    )
                    .into_any_element(),
            );
        }
        let ok = !g.picked.is_empty();
        div()
            .w(px(522.))
            .h(px(640.))
            .max_h(gpui::relative(1.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_header("Tạo cuộc gọi", cx))
            .child(
                div()
                    .px(px(16.))
                    .pt(px(12.))
                    .text_size(px(13.))
                    .text_color(rgb(muted))
                    .child(format!("Nhóm: {}", conv.name)),
            )
            .child(
                div()
                    .mx(px(16.))
                    .mt(px(12.))
                    .h(px(36.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(18.))
                    .border_1()
                    .border_color(crate::color::border_subtle())
                    .text_size(px(14.))
                    .child(fonts::icon_el('\u{ED15}', "⌕", 14.))
                    .child(div().flex_1().child(g.query.clone())),
            )
            .child(
                div()
                    .mx(px(16.))
                    .mt(px(12.))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .text_size(px(13.))
                    .text_color(rgb(muted))
                    .child(format!("{} người", people.len()))
                    .child(
                        div()
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(if g.picked.is_empty() { muted } else { blue }))
                            .child(format!("Đã chọn {}/{max_invites}", g.picked.len())),
                    ),
            )
            .child(
                div()
                    .id("group-call-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .mx(px(16.))
                    .mt(px(8.))
                    .border_t_1()
                    .border_color(crate::color::border_subtle())
                    .children(items),
            )
            .when_some(g.error.clone(), |el, text| {
                el.child(
                    div()
                        .px(px(16.))
                        .py(px(6.))
                        .text_size(px(13.))
                        .text_color(rgb(token_rgb("--R60", "#c31818")))
                        .child(text),
                )
            })
            .child(footer(
                "Hủy",
                "Gọi",
                ok,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.submit_group_call(cx);
                }),
            ))
            .into_any_element()
    }
}

impl ShellView {
    /// Info panel ✎ next to a group name.
    pub(crate) fn open_rename(&mut self, conv: u64, cx: &mut Context<Self>) {
        let name = self.conv(conv).map(|c| c.name.clone()).unwrap_or_default();
        let input = cx.new(|cx| {
            let mut i = LineInput::new(cx, "Nhập tên nhóm mới...");
            i.set_text(name, cx);
            i
        });
        cx.subscribe(&input, |this, _, event: &LineEvent, cx| {
            if matches!(event, LineEvent::Submit(_)) {
                this.submit_rename(cx);
            }
        })
        .detach();
        self.dialog = Some(Dialog::Rename(RenameGroup {
            conv,
            input,
            busy: false,
            error: None,
        }));
    }

    fn submit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Rename(r)) = self.dialog.as_ref() else {
            return;
        };
        let name = r.input.read(cx).text().trim().to_owned();
        if name.is_empty() || r.busy {
            return;
        }
        let id = r.conv;
        let Some(conv) = self.conv(id).cloned() else {
            return;
        };
        let dir = self
            .session_dir
            .clone()
            .filter(|_| self.has_session && conv.live);
        let Some(Dialog::Rename(r)) = self.dialog.as_mut() else {
            return;
        };
        let Some(dir) = dir else {
            r.error = Some("Hội thoại mẫu / chưa đăng nhập".into());
            return;
        };
        r.busy = true;
        r.error = None;
        let grid = conv.thread_id.clone();
        let new_name = name.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.rename_group(&grid, &new_name)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match res {
                    Ok(_) => {
                        view.dialog = None;
                        if let Some(c) = view.convs.iter_mut().find(|c| c.id == id) {
                            c.name = name.clone();
                        }
                        if let Some(g) = view.groups.iter_mut().find(|g| g.uid == conv.thread_id) {
                            g.name = name.clone();
                        }
                        let slot = view.profiles.map.entry(conv.thread_id.clone()).or_default();
                        slot.name = name.clone();
                        if let Some(path) = crate::profiles::cache_path() {
                            view.profiles.save(&path);
                        }
                        crate::data::persist_thread_title(&conv.thread_id, &name);
                        view.set_note(format!("Đã đổi tên nhóm thành «{name}»"));
                    }
                    Err(e) => {
                        if let Some(Dialog::Rename(r)) = view.dialog.as_mut() {
                            r.busy = false;
                            r.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn rename_el(&self, r: &RenameGroup, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let conv = self.conv(r.conv).cloned();
        let person = Person {
            uid: conv
                .as_ref()
                .map(|c| c.thread_id.clone())
                .unwrap_or_default(),
            name: conv.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
            avatar: String::new(),
            note: String::new(),
        };
        div()
            .w(px(400.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_header("Đổi tên nhóm", cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(14.))
                    .px(px(16.))
                    .py(px(20.))
                    .child(self.person_avatar(&person, 56.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(rgb(muted))
                            .text_center()
                            .child(crate::media::wrap_bubble_text(
                                "Bạn có chắc chắn muốn đổi tên nhóm, khi xác nhận tên nhóm mới sẽ hiển thị với tất cả thành viên.",
                                390.,
                            )),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(40.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(rgb(token_rgb("--B55", "#0068ff")))
                            .text_size(px(15.))
                            .child(div().flex_1().child(r.input.clone())),
                    )
                    .when_some(r.error.clone(), |el, e| {
                        el.child(div().text_size(px(13.)).text_color(rgb(token_rgb("--R60", "#c31818"))).child(e))
                    }),
            )
            .child(footer(
                "Hủy",
                if r.busy { "Đang lưu…" } else { "Xác nhận" },
                !r.busy,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.submit_rename(cx);
                    cx.notify();
                }),
            ))
    }

    /// Bubble hover / menu "Share".
    pub(crate) fn open_share(&mut self, msg: crate::data::ChatMsg, cx: &mut Context<Self>) {
        self.open_share_many(vec![msg], cx);
    }

    /// "Share" for one or several messages (multi-select bar).
    pub(crate) fn open_share_many(
        &mut self,
        msgs: Vec<crate::data::ChatMsg>,
        cx: &mut Context<Self>,
    ) {
        let query = cx.new(|cx| LineInput::new(cx, "Tìm kiếm"));
        let note = cx.new(|cx| LineInput::new(cx, "Nhập tin nhắn..."));
        cx.subscribe(&query, |this, _, event: &LineEvent, cx| {
            if let LineEvent::Change(text) = event {
                if let Some(Dialog::Share(s)) = this.dialog.as_mut() {
                    s.query_text = text.clone();
                    cx.notify();
                }
            }
        })
        .detach();
        self.menu = None;
        self.dialog = Some(Dialog::Share(SharePicker {
            msgs,
            query,
            query_text: String::new(),
            picked: Vec::new(),
            note,
        }));
        self.avatar_all = true;
        self.avatars_queued = true;
    }

    /// Send the message to every picked conversation: photos and files are
    /// re-sent from the local copy, everything else as text; the optional
    /// note follows as its own message. The open conversation stays put.
    fn submit_share(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Share(s)) = self.dialog.take() else {
            return;
        };
        let note = s.note.read(cx).text();
        let note_trimmed = note.trim().to_owned();
        let msgs = s.msgs;
        let back = self.selected;
        let dests: Vec<crate::data::ChatConv> = s
            .picked
            .iter()
            .filter_map(|id| self.conv(*id).cloned())
            .collect();
        if dests.is_empty() {
            return;
        }
        let sent = dests.len();
        if dests.len() >= 2 {
            // Batch text forwards via `/api/message/mforward` & `/api/group/mforward`.
            let now = crate::data::now_ms();
            let mut cli_cursor = (now.max(1) as u64).saturating_mul(10);
            let live_targets: Vec<(String, bool)> = dests
                .iter()
                .filter(|d| d.live)
                .map(|d| (d.thread_id.clone(), d.is_group))
                .collect();
            let you = crate::color::str_vi("STR_YOU", "Bạn").to_owned();
            let me_uid = self.self_uid().unwrap_or_default().to_owned();
            for msg in &msgs {
                let local = msg.media_path.clone().filter(|p| p.is_file());
                match (msg.kind, local) {
                    (
                        crate::media::MsgKind::Photo
                        | crate::media::MsgKind::Gif
                        | crate::media::MsgKind::Doodle,
                        Some(path),
                    ) => {
                        for dest in &dests {
                            self.select_conv(dest.id);
                            self.send_photo_now(path.clone(), "");
                        }
                    }
                    (crate::media::MsgKind::File, Some(path)) => {
                        for dest in &dests {
                            self.select_conv(dest.id);
                            self.send_file_now(path.clone());
                        }
                    }
                    _ => {
                        let base_cid = cli_cursor;
                        for (i, dest) in dests.iter().enumerate() {
                            let cid = base_cid.saturating_add(i as u64);
                            self.push_message(crate::data::ChatMsg::text_msg(
                                0,
                                dest.id,
                                dest.thread_id.clone(),
                                true,
                                you.clone(),
                                me_uid.clone(),
                                msg.text.clone(),
                                crate::data::vn_hm(now),
                                now,
                                Some(cid as i64),
                                None,
                            ));
                        }
                        cli_cursor = base_cid.saturating_add(dests.len() as u64 + 1);
                        if !live_targets.is_empty() {
                            self.maybe_live(crate::bridge::LiveAction::ForwardMulti {
                                targets: live_targets.clone(),
                                text: msg.text.clone(),
                                base_client_id: base_cid,
                            });
                        }
                    }
                }
            }
            if !note_trimmed.is_empty() {
                let base_cid = cli_cursor;
                for (i, dest) in dests.iter().enumerate() {
                    let cid = base_cid.saturating_add(i as u64);
                    self.push_message(crate::data::ChatMsg::text_msg(
                        0,
                        dest.id,
                        dest.thread_id.clone(),
                        true,
                        you.clone(),
                        me_uid.clone(),
                        note_trimmed.clone(),
                        crate::data::vn_hm(now),
                        now,
                        Some(cid as i64),
                        None,
                    ));
                }
                if !live_targets.is_empty() {
                    self.maybe_live(crate::bridge::LiveAction::ForwardMulti {
                        targets: live_targets,
                        text: note_trimmed,
                        base_client_id: base_cid,
                    });
                }
            }
        } else {
            let dest = &dests[0];
            self.select_conv(dest.id);
            for msg in &msgs {
                let local = msg.media_path.clone().filter(|p| p.is_file());
                match (msg.kind, local) {
                    (
                        crate::media::MsgKind::Photo
                        | crate::media::MsgKind::Gif
                        | crate::media::MsgKind::Doodle,
                        Some(path),
                    ) => self.send_photo_now(path, ""),
                    (crate::media::MsgKind::File, Some(path)) => self.send_file_now(path),
                    _ => self.forward_to(dest, msg),
                }
            }
            if let (false, Some(first)) = (note_trimmed.is_empty(), msgs.first()) {
                let mut extra = first.clone();
                extra.kind = crate::media::MsgKind::Text;
                extra.text = note_trimmed;
                self.forward_to(dest, &extra);
            }
        }
        if let Some(id) = back {
            self.select_conv(id);
        }
        self.set_note(format!("Đã chia sẻ tới {sent} hội thoại"));
    }

    fn share_el(&self, s: &SharePicker, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::media::MsgKind;
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
        let q = s.query_text.trim().to_lowercase();
        let convs: Vec<_> = self
            .listed_convs()
            .into_iter()
            .filter(|c| q.is_empty() || c.name.to_lowercase().contains(&q))
            .collect();
        let picked: HashSet<u64> = s.picked.iter().copied().collect();
        let first = s.msgs.first().cloned().unwrap_or_default();
        let preview = match first.kind {
            MsgKind::Photo | MsgKind::Gif => "[Hình ảnh]".to_owned(),
            MsgKind::Doodle => "[Vẽ hình]".to_owned(),
            MsgKind::Location => "[Vị trí]".to_owned(),
            MsgKind::Ecard => "[Thiệp]".to_owned(),
            MsgKind::File => format!("[File] {}", first.file_name),
            MsgKind::Sticker => "[Sticker]".to_owned(),
            _ => first.text.clone(),
        };
        let preview = if s.msgs.len() > 1 {
            format!("{preview} và {} tin nhắn khác", s.msgs.len() - 1)
        } else {
            preview
        };
        let thumb = self.media_bitmaps.get(&first.id).cloned();
        let ok = !s.picked.is_empty();
        div()
            .w(px(522.))
            .h(px(740.))
            .max_h(gpui::relative(1.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_header("Chia sẻ", cx))
            .child(
                div().px(px(16.)).pt(px(14.)).child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .h(px(36.))
                        .px(px(12.))
                        .rounded(px(18.))
                        .border_1()
                        .border_color(crate::color::border_subtle())
                        .child(fonts::icon_el('\u{ED15}', "⌕", 16.))
                        .child(div().flex_1().text_size(px(14.)).child(s.query.clone())),
                ),
            )
            .child(
                div()
                    .px(px(16.))
                    .pt(px(12.))
                    .pb(px(4.))
                    .text_size(px(14.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child("Trò chuyện gần đây"),
            )
            .child(
                div()
                    .id("share-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(px(6.))
                    .children(convs.into_iter().map(|c| {
                        let on = picked.contains(&c.id);
                        let id = c.id;
                        let person = Person {
                            uid: c.thread_id.clone(),
                            name: c.name.clone(),
                            avatar: String::new(),
                            note: String::new(),
                        };
                        div()
                            .id(("share-pick", c.id))
                            .flex()
                            .flex_none()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .h(px(52.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|st| {
                                st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4")))
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(Dialog::Share(s)) = this.dialog.as_mut() {
                                    if let Some(i) = s.picked.iter().position(|p| *p == id) {
                                        s.picked.remove(i);
                                    } else {
                                        s.picked.push(id);
                                    }
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_color(rgb(if on {
                                        blue
                                    } else {
                                        token_rgb("--text-disabled", "#8b96a7")
                                    }))
                                    .child(fonts::icon_el(
                                        if on { '\u{ECEF}' } else { '\u{EDD3}' },
                                        "○",
                                        22.,
                                    )),
                            )
                            .child(self.person_avatar(&person, 40.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(14.))
                                    .truncate()
                                    .child(c.name.clone()),
                            )
                    })),
            )
            .child(
                div()
                    .mx(px(16.))
                    .mt(px(8.))
                    .p(px(12.))
                    .rounded(px(6.))
                    .bg(rgb(field))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_family(FAMILY_SEMIBOLD)
                            .child("Chia sẻ tin nhắn"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.))
                            .when_some(thumb, |el, t| {
                                el.child(
                                    img(t)
                                        .size(px(48.))
                                        .rounded(px(4.))
                                        .object_fit(gpui::ObjectFit::Cover),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(13.))
                                    .text_color(rgb(muted))
                                    .truncate()
                                    .child(preview),
                            ),
                    ),
            )
            .child(
                div()
                    .mx(px(16.))
                    .mt(px(10.))
                    .h(px(40.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(crate::color::border_subtle())
                    .text_size(px(14.))
                    .child(div().flex_1().child(s.note.clone())),
            )
            .child(footer(
                "Hủy",
                "Chia sẻ",
                ok,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.submit_share(cx);
                    cx.notify();
                }),
            ))
    }

    /// "Message Details": sender, full send time, type, size, ids.
    fn about_el(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let link_fg = token_rgb("--text-information", "#005ae0");
        let panel_bg = token_rgb("--layer-background-subtle", "#ebecf0");
        let badge_bg = token_rgb("--accent-blue-bg-subtle", "#f0f7ff");
        let badge_fg = token_rgb("--accent-blue-text", "#005ae0");
        let primary_bg = token_rgb("--button-primary-normal", "#0068ff");
        let primary_hover = token_rgb("--button-primary-hover", "#0045ad");
        let primary_fg = token_rgb("--button-primary-text", "#ffffff");

        let proto = zalo_protocol::active_config();
        let build_info = if crate::APP_GIT_SHA.is_empty() {
            crate::APP_BUILD_PROFILE.to_owned()
        } else {
            format!("{} · {}", crate::APP_BUILD_PROFILE, crate::APP_GIT_SHA)
        };
        let os_name = match std::env::consts::OS {
            "linux" => "Linux",
            "macos" => "macOS",
            "windows" => "Windows",
            other => other,
        };
        let platform_info = format!("{os_name} ({})", std::env::consts::ARCH);
        let protocol_info = format!(
            "Zalo PC {} (client v{}, type {})",
            proto.zalo_desktop_version, proto.client_version, proto.api_type
        );
        let repo_url = crate::APP_REPO_URL;
        let repo_label = repo_url
            .strip_prefix("https://")
            .or_else(|| repo_url.strip_prefix("http://"))
            .unwrap_or(repo_url);

        let info_rows: Vec<(&'static str, String)> = vec![
            ("Phiên bản", crate::APP_VERSION_TAG.to_owned()),
            ("Bản dựng", build_info),
            ("Giao thức", protocol_info),
            ("Hệ điều hành", platform_info),
            ("Nền tảng", "Rust + GPUI".to_owned()),
            ("Giấy phép", "MIT".to_owned()),
        ];

        let icon = crate::titlebar::zalo_title_icon();

        modal_card(448.)
            .child(self.dialog_title_bar(
                crate::color::str_vi("STR_VERSION_INFO", "Thông tin phiên bản"),
                cx,
            ))
            .child(
                div()
                    .w_full()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(14.))
                            .when_some(icon, |el, bitmap| {
                                el.child(
                                    img(bitmap)
                                        .size(px(48.))
                                        .flex_none()
                                        .object_fit(gpui::ObjectFit::Contain),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.))
                                    .min_w(px(0.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap(px(8.))
                                            .child(
                                                div()
                                                    .text_size(px(17.))
                                                    .font_family(FAMILY_SEMIBOLD)
                                                    .text_color(rgb(ink))
                                                    .child("Zalo Rust"),
                                            )
                                            .child(
                                                div()
                                                    .px(px(8.))
                                                    .py(px(2.))
                                                    .rounded(px(10.))
                                                    .bg(rgb(badge_bg))
                                                    .text_size(px(12.))
                                                    .font_family(FAMILY_SEMIBOLD)
                                                    .text_color(rgb(badge_fg))
                                                    .child(crate::APP_VERSION_TAG),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(rgb(muted))
                                            .child(crate::i18n::t(
                                                "Ứng dụng Zalo gốc viết bằng Rust & GPUI",
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .rounded(px(8.))
                            .bg(rgb(panel_bg))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .px(px(14.))
                            .py(px(10.))
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .children(info_rows.into_iter().map(|(label, value)| {
                                div()
                                    .w_full()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(12.))
                                    .text_size(px(13.))
                                    .child(
                                        div()
                                            .w(px(104.))
                                            .flex_none()
                                            .text_color(rgb(muted))
                                            .child(crate::i18n::t(label)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .text_color(rgb(ink))
                                            .child(value),
                                    )
                            }))
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(12.))
                                    .text_size(px(13.))
                                    .child(
                                        div()
                                            .w(px(104.))
                                            .flex_none()
                                            .text_color(rgb(muted))
                                            .child(crate::i18n::t("Mã nguồn")),
                                    )
                                    .child(
                                        div()
                                            .id("about-repo-link")
                                            .flex_1()
                                            .min_w(px(0.))
                                            .text_color(rgb(link_fg))
                                            .cursor_pointer()
                                            .on_click(move |_, _, cx| {
                                                cx.open_url(repo_url);
                                            })
                                            .child(repo_label),
                                    ),
                            ),
                    )
                    .when_some(self.update_info.clone(), |el, info| {
                        let tag = info.tag.clone();
                        let summary = info.summary.clone();
                        let pkg_badge = info.package_badge();
                        el.child(
                            div()
                                .w_full()
                                .rounded(px(8.))
                                .bg(rgb(badge_bg))
                                .border_1()
                                .border_color(crate::color::border_subtle())
                                .px(px(14.))
                                .py(px(10.))
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .gap(px(12.))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.))
                                        .min_w(px(0.))
                                        .flex_1()
                                        .child(
                                            div()
                                                .text_size(px(13.))
                                                .font_family(FAMILY_SEMIBOLD)
                                                .text_color(rgb(badge_fg))
                                                .child(format!(
                                                    "{} {tag}",
                                                    crate::i18n::t("Có phiên bản mới:")
                                                )),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .text_color(rgb(muted))
                                                .truncate()
                                                .child(if summary.is_empty() {
                                                    pkg_badge
                                                } else {
                                                    summary
                                                }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("about-install-update-btn")
                                        .flex_none()
                                        .px(px(12.))
                                        .h(px(30.))
                                        .rounded(px(4.))
                                        .bg(rgb(primary_bg))
                                        .hover(move |s| s.bg(rgb(primary_hover)))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(12.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(primary_fg))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.dialog = Some(Dialog::UpdateAvailable);
                                            cx.notify();
                                        }))
                                        .child(crate::i18n::t("Cập nhật ngay")),
                                ),
                        )
                    })
                    .when(self.update_info.is_none(), |el| {
                        let checking = self.update_checking;
                        let note = self.update_checked_note.clone();
                        el.child(
                            div()
                                .w_full()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .text_size(px(12.5))
                                .child(div().text_color(rgb(muted)).child(if checking {
                                    crate::i18n::t("Đang kiểm tra cập nhật...")
                                } else if let Some(msg) = note {
                                    msg
                                } else {
                                    crate::i18n::t("Kiểm tra phiên bản mới từ GitHub Releases")
                                }))
                                .when(!checking, |d| {
                                    d.child(
                                        div()
                                            .id("about-check-update-btn")
                                            .text_color(rgb(link_fg))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.trigger_update_check(true, cx);
                                            }))
                                            .child(crate::i18n::t("Kiểm tra cập nhật")),
                                    )
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .px(px(16.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(crate::color::border_subtle())
                    .child(
                        div()
                            .id("about-close-btn")
                            .px(px(18.))
                            .h(px(34.))
                            .rounded(px(4.))
                            .bg(rgb(primary_bg))
                            .hover(move |s| s.bg(rgb(primary_hover)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(13.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(primary_fg))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialog = None;
                                cx.notify();
                            }))
                            .child(crate::color::str_vi("STR_CLOSE", "Đóng")),
                    ),
            )
    }

    fn update_available_el(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let link_fg = token_rgb("--text-information", "#005ae0");
        let panel_bg = token_rgb("--layer-background-subtle", "#ebecf0");
        let badge_bg = token_rgb("--accent-blue-bg-subtle", "#f0f7ff");
        let primary_bg = crate::color::accent_bg();
        let primary_hover = token_rgb("--button-primary-hover", "#005ae0");
        let primary_fg = crate::color::on_accent_fg();
        let sec_bg = token_rgb("--button-secondary-neutral-normal", "#e5e7eb");
        let sec_hover = token_rgb("--button-secondary-neutral-hover", "#d6dbe1");
        let hover_bg = token_rgb("--layer-background-hover", "#f1f2f4");
        let err_fg = token_rgb("--text-errors", "#d91b1b");
        let track_bg = if crate::color::is_dark() {
            0x2c_2c_2c
        } else {
            0xe4_e6_eb
        };

        let Some(info) = self.update_info.clone() else {
            return div().w(px(1.));
        };

        let install_state = self.update_install.clone();
        let busy = install_state.is_busy();
        let supports_in_app = info.package_kind.supports_in_app_install();
        let pkg_badge = info.package_badge();
        let release_url = info.release_url.clone();
        let manual_url = info.download_url.clone();
        let tag = info.tag.clone();

        let notes_text = if !info.notes.trim().is_empty() {
            info.notes.clone()
        } else if !info.summary.trim().is_empty() {
            info.summary.clone()
        } else {
            crate::i18n::t("Phiên bản mới đã sẵn sàng để tải và cài đặt tự động.").to_owned()
        };
        let wrapped_notes: Vec<String> = notes_text
            .lines()
            .flat_map(|line| {
                crate::media::wrap_text_at(line.trim(), 384., 12.5, false)
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .take(12)
            .collect();

        let (progress_label, progress_detail, is_error) = match &install_state {
            crate::updater::UpdateInstallState::Idle => (String::new(), String::new(), false),
            crate::updater::UpdateInstallState::Downloading { downloaded, total } => {
                let detail = match total {
                    Some(t) if *t > 0 => {
                        let pct = ((*downloaded as f64 / *t as f64) * 100.0).round() as u64;
                        format!(
                            "{} / {} ({}%)",
                            crate::updater::format_bytes(*downloaded),
                            crate::updater::format_bytes(*t),
                            pct.min(100)
                        )
                    }
                    _ => crate::updater::format_bytes(*downloaded),
                };
                (
                    crate::i18n::t("Đang tải bản cập nhật...").to_owned(),
                    detail,
                    false,
                )
            }
            crate::updater::UpdateInstallState::Verifying => (
                crate::i18n::t("Đang xác minh toàn vẹn SHA-256...").to_owned(),
                "84%".to_owned(),
                false,
            ),
            crate::updater::UpdateInstallState::Installing { message } => {
                (message.clone(), "92%".to_owned(), false)
            }
            crate::updater::UpdateInstallState::Restarting { message } => {
                (message.clone(), "100%".to_owned(), false)
            }
            crate::updater::UpdateInstallState::Failed(err) => (err.clone(), String::new(), true),
        };
        let progress_frac = install_state.progress_fraction();
        let show_progress = !matches!(install_state, crate::updater::UpdateInstallState::Idle);

        let cta_label = match &install_state {
            crate::updater::UpdateInstallState::Idle => {
                if supports_in_app {
                    crate::i18n::t("Cập nhật & khởi động lại").to_owned()
                } else {
                    crate::i18n::t("Mở trang tải về").to_owned()
                }
            }
            crate::updater::UpdateInstallState::Downloading { downloaded, total } => match total {
                Some(t) if *t > 0 => {
                    let pct = ((*downloaded as f64 / *t as f64) * 100.0).round() as u64;
                    format!("{}%", pct.min(100))
                }
                _ => "...".to_owned(),
            },
            crate::updater::UpdateInstallState::Verifying => "SHA-256...".to_owned(),
            crate::updater::UpdateInstallState::Installing { .. } => {
                crate::i18n::t("Đang cài đặt...").to_owned()
            }
            crate::updater::UpdateInstallState::Restarting { .. } => {
                crate::i18n::t("Đang khởi động lại...").to_owned()
            }
            crate::updater::UpdateInstallState::Failed(_) => crate::i18n::t("Thử lại").to_owned(),
        };

        div()
            .w(px(464.))
            .max_w(px(520.))
            .rounded(px(10.))
            .overflow_hidden()
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .border_1()
            .border_color(crate::color::border_subtle())
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            // Header
            .child(
                div()
                    .w_full()
                    .h(px(52.))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(crate::color::border_subtle())
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.))
                            .when_some(crate::titlebar::zalo_title_icon(), |el, icon| {
                                el.child(
                                    img(icon)
                                        .size(px(24.))
                                        .rounded(px(6.))
                                        .object_fit(gpui::ObjectFit::Contain),
                                )
                            })
                            .child(
                                div()
                                    .text_size(px(15.5))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(ink))
                                    .child(crate::i18n::t("Có bản cập nhật mới")),
                            ),
                    )
                    .when(!busy, |el| {
                        el.child(
                            div()
                                .id("update-popup-close")
                                .size(px(28.))
                                .rounded(px(6.))
                                .hover(move |s| s.bg(rgb(hover_bg)))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_size(px(16.))
                                .text_color(rgb(muted))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dialog = None;
                                    cx.notify();
                                }))
                                .child("×"),
                        )
                    }),
            )
            // Body
            .child(
                div()
                    .w_full()
                    .px(px(18.))
                    .py(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    // Version comparison & package badge
                    .child(
                        div()
                            .w_full()
                            .rounded(px(8.))
                            .bg(rgb(panel_bg))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .px(px(14.))
                            .py(px(12.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .px(px(8.))
                                            .py(px(3.))
                                            .rounded(px(6.))
                                            .bg(rgb(badge_bg))
                                            .text_size(px(12.5))
                                            .text_color(rgb(muted))
                                            .child(crate::APP_VERSION_TAG),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(rgb(muted))
                                            .child("→"),
                                    )
                                    .child(
                                        div()
                                            .px(px(9.))
                                            .py(px(3.))
                                            .rounded(px(6.))
                                            .bg(rgb(primary_bg))
                                            .text_size(px(12.5))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(primary_fg))
                                            .child(tag),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(muted))
                                    .child(pkg_badge),
                            ),
                    )
                    // Release notes
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(muted))
                                            .child(crate::i18n::t("Có gì mới")),
                                    )
                                    .child(
                                        div()
                                            .id("update-open-github")
                                            .text_size(px(12.))
                                            .text_color(rgb(link_fg))
                                            .cursor_pointer()
                                            .on_click(move |_, _, cx| {
                                                cx.open_url(&release_url);
                                            })
                                            .child(crate::i18n::t("Xem trên GitHub")),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .rounded(px(8.))
                                    .bg(rgb(panel_bg))
                                    .border_1()
                                    .border_color(crate::color::border_subtle())
                                    .px(px(12.))
                                    .py(px(10.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .children(wrapped_notes.into_iter().map(|line| {
                                        div()
                                            .text_size(px(12.5))
                                            .line_height(px(18.))
                                            .text_color(rgb(ink))
                                            .child(line)
                                    })),
                            ),
                    )
                    // Progress / Error box
                    .when(show_progress, |el| {
                        let fallback_url = manual_url.clone();
                        el.child(
                            div()
                                .w_full()
                                .rounded(px(8.))
                                .bg(rgb(panel_bg))
                                .border_1()
                                .border_color(crate::color::border_subtle())
                                .px(px(12.))
                                .py(px(10.))
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .w_full()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(8.))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .text_size(px(12.5))
                                                .text_color(rgb(if is_error { err_fg } else { ink }))
                                                .child(progress_label),
                                        )
                                        .when(!progress_detail.is_empty(), |d| {
                                            d.child(
                                                div()
                                                    .flex_none()
                                                    .text_size(px(12.))
                                                    .font_family(FAMILY_SEMIBOLD)
                                                    .text_color(rgb(muted))
                                                    .child(progress_detail),
                                            )
                                        }),
                                )
                                .when(!is_error, |d| {
                                    d.child(
                                        div()
                                            .w_full()
                                            .h(px(6.))
                                            .rounded(px(3.))
                                            .bg(rgb(track_bg))
                                            .overflow_hidden()
                                            .child(
                                                div()
                                                    .h_full()
                                                    .w(gpui::relative(progress_frac))
                                                    .rounded(px(3.))
                                                    .bg(rgb(primary_bg)),
                                            ),
                                    )
                                })
                                .when(is_error, |d| {
                                    d.child(
                                        div()
                                            .id("update-manual-fallback")
                                            .text_size(px(12.))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(link_fg))
                                            .cursor_pointer()
                                            .on_click(move |_, _, cx| {
                                                cx.open_url(&fallback_url);
                                            })
                                            .child(crate::i18n::t("Mở trang tải thủ công")),
                                    )
                                }),
                        )
                    }),
            )
            // Footer
            .child(
                div()
                    .w_full()
                    .px(px(16.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(crate::color::border_subtle())
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .when(!busy, |d| {
                                d.child(
                                    div()
                                        .id("update-skip-version")
                                        .text_size(px(12.))
                                        .text_color(rgb(muted))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.dismiss_update_version(cx);
                                        }))
                                        .child(crate::i18n::t("Bỏ qua phiên bản này")),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .when(!busy, |d| {
                                d.child(
                                    div()
                                        .id("update-later-btn")
                                        .px(px(14.))
                                        .h(px(34.))
                                        .rounded(px(6.))
                                        .bg(rgb(sec_bg))
                                        .hover(move |s| s.bg(rgb(sec_hover)))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(13.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(ink))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.dialog = None;
                                            cx.notify();
                                        }))
                                        .child(crate::i18n::t("Để sau")),
                                )
                            })
                            .child(
                                div()
                                    .id("update-install-now-btn")
                                    .px(px(16.))
                                    .h(px(34.))
                                    .rounded(px(6.))
                                    .bg(rgb(if busy { sec_bg } else { primary_bg }))
                                    .when(!busy, |d| {
                                        d.hover(move |s| s.bg(rgb(primary_hover)))
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.start_update_install(cx);
                                            }))
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(if busy { muted } else { primary_fg }))
                                    .child(cta_label),
                            ),
                    ),
            )
    }

    fn contact_el(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = token_rgb("--text-secondary", "#5a6981");
        let link_fg = token_rgb("--text-information", "#005ae0");
        let panel_bg = token_rgb("--layer-background-subtle", "#ebecf0");
        let primary_bg = token_rgb("--button-primary-normal", "#0068ff");
        let primary_hover = token_rgb("--button-primary-hover", "#0045ad");
        let primary_fg = token_rgb("--button-primary-text", "#ffffff");

        let contacts: [(&'static str, &'static str, &'static str); 5] = [
            ("Email", "lienhe@truong.it", "mailto:lienhe@truong.it"),
            ("Telegram", "https://t.me/congtruongit", "https://t.me/congtruongit"),
            ("Facebook", "https://m.me/congtruongit", "https://m.me/congtruongit"),
            ("Donate", "https://truong.me/donate", "https://truong.me/donate"),
            ("Website", "https://lza.lol", "https://lza.lol"),
        ];

        modal_card(448.)
            .child(self.dialog_title_bar(
                crate::color::str_vi("STR_FEEDBACK", "Liên hệ"),
                cx,
            ))
            .child(
                div()
                    .w_full()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(
                        div()
                            .w_full()
                            .whitespace_normal()
                            .text_size(px(13.))
                            .line_height(px(19.))
                            .text_color(rgb(muted))
                            .child(crate::i18n::t(
                                "Liên hệ hỗ trợ, báo lỗi hoặc đóng góp cho dự án Zalo Rust",
                            )),
                    )
                    .child(
                        div()
                            .w_full()
                            .rounded(px(8.))
                            .bg(rgb(panel_bg))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .px(px(14.))
                            .py(px(10.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .children(contacts.into_iter().enumerate().map(
                                |(idx, (label, display, url))| {
                                    div()
                                        .w_full()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(12.))
                                        .text_size(px(13.))
                                        .child(
                                            div()
                                                .w(px(84.))
                                                .flex_none()
                                                .text_color(rgb(muted))
                                                .child(label),
                                        )
                                        .child(
                                            div()
                                                .id(("contact-link", idx))
                                                .flex_1()
                                                .min_w(px(0.))
                                                .text_color(rgb(link_fg))
                                                .cursor_pointer()
                                                .on_click(move |_, _, cx| {
                                                    cx.open_url(url);
                                                })
                                                .child(display),
                                        )
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .px(px(16.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(crate::color::border_subtle())
                    .child(
                        div()
                            .id("contact-close-btn")
                            .px(px(18.))
                            .h(px(34.))
                            .rounded(px(4.))
                            .bg(rgb(primary_bg))
                            .hover(move |s| s.bg(rgb(primary_hover)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(13.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(primary_fg))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialog = None;
                                cx.notify();
                            }))
                            .child(crate::color::str_vi("STR_CLOSE", "Đóng")),
                    ),
            )
    }

    fn details_el(&self, m: &crate::data::ChatMsg, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::media::MsgKind;
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let when = if m.at_ms > 0 {
            let (y, mo, d) = crate::data::vn_ymd(m.at_ms);
            format!("{} {d:02}/{mo:02}/{y}", crate::data::vn_hm(m.at_ms))
        } else {
            m.time.clone()
        };
        let kind = match m.kind {
            MsgKind::Text => "Tin nhắn văn bản",
            MsgKind::Photo => "Hình ảnh",
            MsgKind::Doodle => "Vẽ hình",
            MsgKind::Location => "Vị trí",
            MsgKind::Ecard => "Thiệp",
            MsgKind::Sticker => "Sticker",
            MsgKind::Gif => "GIF",
            MsgKind::File => "Tệp tin",
            MsgKind::Link => "Liên kết",
            MsgKind::Call | MsgKind::MissedCall => "Cuộc gọi",
            MsgKind::Voice => "Tin nhắn thoại",
            _ => "Tin nhắn",
        };
        let mut rows: Vec<(&'static str, String)> = vec![
            (
                "Người gửi",
                if m.from_me {
                    "Bạn".to_owned()
                } else {
                    m.sender.clone()
                },
            ),
            ("Thời gian gửi", when),
            ("Loại", kind.to_owned()),
        ];
        if m.media_w > 0 && m.media_h > 0 {
            rows.push(("Kích thước", format!("{} × {} px", m.media_w, m.media_h)));
        }
        if !m.file_name.is_empty() {
            rows.push(("Tên tệp", m.file_name.clone()));
        }
        if m.file_size > 0 {
            rows.push(("Dung lượng", crate::settings::fmt_size(m.file_size)));
        }
        if let Some(path) = m.media_path.as_ref().filter(|p| p.exists()) {
            rows.push(("Trên máy", path.display().to_string()));
        }
        rows.push((
            "Trạng thái",
            if m.recalled {
                "Đã thu hồi".into()
            } else {
                "Đã gửi".into()
            },
        ));
        let sender = Person {
            uid: if m.from_me {
                self.uin.clone().unwrap_or_default()
            } else {
                m.sender_uid.clone()
            },
            name: m.sender.clone(),
            avatar: String::new(),
            note: String::new(),
        };
        div()
            .w(px(400.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(rgb(crate::color::token_rgb(
                "--layer-background",
                "#ffffff",
            )))
            .text_color(rgb(ink))
            .shadow_lg()
            .child(self.dialog_header("Chi tiết tin nhắn", cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .px(px(16.))
                    .pt(px(16.))
                    .child(self.person_avatar(&sender, 40.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .child(sender.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(muted))
                                    .truncate()
                                    .child(if m.text.is_empty() {
                                        kind.to_owned()
                                    } else {
                                        m.text.clone()
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .p(px(16.))
                    .children(rows.into_iter().map(|(k, v)| {
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(12.))
                            .text_size(px(14.))
                            .child(
                                div()
                                    .w(px(110.))
                                    .flex_none()
                                    .text_color(rgb(muted))
                                    .child(k),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(crate::media::wrap_bubble_text(&v, 240.0)),
                            )
                    })),
            )
    }

    /// Composer "Send Contact Card": opens contact picker dialog.
    pub(crate) fn open_namecard_picker(&mut self, cx: &mut Context<Self>) {
        let query = cx.new(|cx| LineInput::new(cx, "Tìm bạn bè để gửi danh thiếp…"));
        cx.subscribe(&query, |this, _, event: &LineEvent, cx| {
            if let LineEvent::Change(text) = event {
                if let Some(Dialog::Namecard(n)) = this.dialog.as_mut() {
                    n.query_text = text.clone();
                    cx.notify();
                }
            }
        })
        .detach();
        self.dialog = Some(Dialog::Namecard(NamecardPicker {
            query,
            query_text: String::new(),
        }));
        self.avatar_all = true;
        self.avatars_queued = true;
    }

    fn namecard_contacts(&self) -> Vec<Person> {
        let mut all = self.people().1;
        for (idx, f) in self.friends.iter().enumerate() {
            if !f.name.is_empty()
                && f.name != "Zalo"
                && !crate::shell::is_my_documents(&f.name, &f.uid, self.send2me_id.as_deref())
                && !all
                    .iter()
                    .any(|p| (!f.uid.is_empty() && p.uid == f.uid) || p.name == f.name)
            {
                all.push(Person {
                    uid: if f.uid.is_empty() {
                        format!("preview:{}", idx + 1)
                    } else {
                        f.uid.clone()
                    },
                    name: f.name.clone(),
                    avatar: String::new(),
                    note: if f.uid.is_empty() {
                        if f.online {
                            "Đang hoạt động".into()
                        } else {
                            String::new()
                        }
                    } else if self.uid_online(&f.uid) {
                        "Vừa truy cập".into()
                    } else {
                        String::new()
                    },
                });
            }
        }
        if all.is_empty() {
            for c in self.convs.iter().filter(|c| {
                !c.is_group
                    && !crate::shell::is_my_documents(
                        &c.name,
                        &c.thread_id,
                        self.send2me_id.as_deref(),
                    )
                    && c.name != "Zalo"
            }) {
                all.push(Person {
                    uid: c.thread_id.clone(),
                    name: c.name.clone(),
                    avatar: String::new(),
                    note: String::new(),
                });
            }
        }
        all.sort_by_key(|p| p.name.to_lowercase());
        all
    }

    fn send_namecard(&mut self, person: &Person, _cx: &mut Context<Self>) {
        let Some(conv) = self.selected_conv() else {
            self.set_note("Chọn một hội thoại để gửi");
            return;
        };
        let now = crate::data::now_ms();
        let cli = now.max(1) as u64;
        let id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.saturating_add(1);
        let mut msg = crate::data::ChatMsg::text_msg(
            id,
            conv.id,
            conv.thread_id.clone(),
            true,
            crate::color::str_vi("STR_YOU", "Bạn").to_owned(),
            self.uin.clone().unwrap_or_default(),
            format!("[Danh thiếp] {}", person.name),
            crate::data::vn_hm(now),
            now,
            Some(cli as i64),
            None,
        );
        msg.kind = crate::media::MsgKind::Card;
        msg.file_name = person.name.clone();
        self.push_message(msg);
        if conv.live && self.has_session && !person.uid.starts_with("preview:") {
            self.maybe_live(crate::bridge::LiveAction::SendContact {
                thread_id: conv.thread_id,
                is_group: conv.is_group,
                contact_uid: person.uid.clone(),
                client_id: cli,
            });
        }
    }

    fn namecard_el(&self, d: &NamecardPicker, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let blue = token_rgb("--B55", "#0068ff");
        let q = d.query_text.trim().to_lowercase();
        let contacts: Vec<Person> = self
            .namecard_contacts()
            .into_iter()
            .filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q) || p.uid.contains(&q))
            .collect();
        div()
            .w(px(440.))
            .h(px(560.))
            .max_h(gpui::relative(1.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_header("Gửi danh thiếp", cx))
            .child(
                div().px(px(16.)).py(px(12.)).child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .h(px(36.))
                        .px(px(12.))
                        .rounded(px(18.))
                        .border_1()
                        .border_color(crate::color::border_subtle())
                        .child(fonts::icon_el('\u{ED15}', "⌕", 16.))
                        .child(div().flex_1().text_size(px(14.)).child(d.query.clone())),
                ),
            )
            .child(
                div()
                    .id("namecard-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(px(8.))
                    .pb(px(12.))
                    .when(contacts.is_empty(), |el| {
                        el.child(
                            div()
                                .py(px(28.))
                                .text_center()
                                .text_size(px(14.))
                                .text_color(rgb(muted))
                                .child("Không tìm thấy bạn bè phù hợp"),
                        )
                    })
                    .children(contacts.into_iter().enumerate().map(|(idx, p)| {
                        let pick = p.clone();
                        div()
                            .id(("card-pick", idx as u64))
                            .flex()
                            .flex_none()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .h(px(54.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|st| st.bg(rgb(hover)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_namecard(&pick, cx);
                                this.dialog = None;
                                cx.notify();
                            }))
                            .child(self.person_avatar(&p, 38.))
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
                                            .truncate()
                                            .child(p.name),
                                    )
                                    .when(!p.note.is_empty(), |el| {
                                        el.child(
                                            div()
                                                .text_size(px(12.))
                                                .text_color(rgb(muted))
                                                .child(p.note),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(blue))
                                    .child("Gửi"),
                            )
                    })),
            )
    }

    /// Composer "Send Bank Account": opens transfer card form pre-filled from prefs.
    pub(crate) fn open_bank_card_dialog(&mut self, cx: &mut Context<Self>) {
        let saved_bank = self.prefs.str("bankName", "");
        let saved_acc = self.prefs.str("bankAcc", "");
        let saved_owner = {
            let s = self.prefs.str("bankOwner", "");
            if s.is_empty() {
                self.display_name.clone().unwrap_or_default()
            } else {
                s
            }
        };
        let b_init = saved_bank.clone();
        let a_init = saved_acc.clone();
        let bank = cx.new(|cx| {
            let mut i = LineInput::new(cx, "VD: Vietcombank, MBBank, Techcombank…");
            if !b_init.is_empty() {
                i.set_text(b_init, cx);
            }
            i
        });
        let account = cx.new(|cx| {
            let mut i = LineInput::new(cx, "Số tài khoản ngân hàng");
            if !a_init.is_empty() {
                i.set_text(a_init, cx);
            }
            i
        });
        let owner = cx.new(|cx| {
            let mut i = LineInput::new(cx, "Tên chủ tài khoản");
            if !saved_owner.is_empty() {
                i.set_text(saved_owner, cx);
            }
            i
        });
        let amount = cx.new(|cx| LineInput::new(cx, "Số tiền (tùy chọn, VD: 200.000đ)"));
        let note = cx.new(|cx| LineInput::new(cx, "Nội dung chuyển khoản (tùy chọn)"));
        cx.subscribe(&bank, |this, _, ev: &LineEvent, cx| {
            if let LineEvent::Change(t) = ev {
                if let Some(Dialog::BankCard(b)) = this.dialog.as_mut() {
                    b.bank_text = t.clone();
                    cx.notify();
                }
            }
        })
        .detach();
        cx.subscribe(&account, |this, _, ev: &LineEvent, cx| {
            if let LineEvent::Change(t) = ev {
                if let Some(Dialog::BankCard(b)) = this.dialog.as_mut() {
                    b.acc_text = t.clone();
                    cx.notify();
                }
            }
        })
        .detach();
        self.dialog = Some(Dialog::BankCard(BankCardDlg {
            bank,
            bank_text: saved_bank,
            account,
            acc_text: saved_acc,
            owner,
            amount,
            note,
        }));
    }

    fn submit_bank_card(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::BankCard(b)) = self.dialog.as_ref() else {
            return;
        };
        let bank = b.bank.read(cx).text().trim().to_owned();
        let acc = b.account.read(cx).text().trim().to_owned();
        let owner = b.owner.read(cx).text().trim().to_owned();
        let amount = b.amount.read(cx).text().trim().to_owned();
        let memo = b.note.read(cx).text().trim().to_owned();
        if bank.is_empty() || acc.is_empty() {
            return;
        }
        self.prefs
            .set("bankName", serde_json::Value::String(bank.clone()));
        self.prefs
            .set("bankAcc", serde_json::Value::String(acc.clone()));
        if !owner.is_empty() {
            self.prefs
                .set("bankOwner", serde_json::Value::String(owner.clone()));
        }
        let body = format_bank_card_message(&bank, &acc, &owner, &amount, &memo);
        self.send_draft(&body, cx);
        self.dialog = None;
    }

    fn bank_card_el(&self, b: &BankCardDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let chip_bg = token_rgb("--layer-background-hover", "#f1f2f4");
        let ok = !b.bank_text.trim().is_empty() && !b.acc_text.trim().is_empty();
        let banks = [
            "Vietcombank",
            "MBBank",
            "Techcombank",
            "BIDV",
            "TPBank",
            "ACB",
            "VPBank",
        ];
        let field_box = |label: &'static str, input: Entity<LineInput>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(muted))
                        .child(label),
                )
                .child(
                    div()
                        .h(px(36.))
                        .px(px(12.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(crate::color::border_subtle())
                        .flex()
                        .items_center()
                        .text_size(px(14.))
                        .child(input),
                )
        };
        div()
            .w(px(460.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .text_color(rgb(ink))
            .shadow_lg()
            .child(self.dialog_header("Gửi tài khoản ngân hàng", cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .p(px(16.))
                    .child(div().flex().flex_row().flex_wrap().gap(px(6.)).children(
                        banks.into_iter().enumerate().map(|(i, name)| {
                            div()
                                .id(("bank-chip", i as u64))
                                .px(px(10.))
                                .h(px(26.))
                                .rounded(px(13.))
                                .bg(rgb(chip_bg))
                                .flex()
                                .items_center()
                                .text_size(px(12.))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(Dialog::BankCard(dlg)) = this.dialog.as_mut() {
                                        dlg.bank_text = name.to_owned();
                                        dlg.bank.update(cx, |inp, cx| {
                                            inp.set_text(name.to_owned(), cx)
                                        });
                                        cx.notify();
                                    }
                                }))
                                .child(name)
                        }),
                    ))
                    .child(field_box("Ngân hàng *", b.bank.clone()))
                    .child(field_box("Số tài khoản *", b.account.clone()))
                    .child(field_box("Chủ tài khoản", b.owner.clone()))
                    .child(field_box("Số tiền", b.amount.clone()))
                    .child(field_box("Nội dung chuyển khoản", b.note.clone())),
            )
            .child(footer(
                "Hủy",
                "Gửi thông tin",
                ok,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.submit_bank_card(cx);
                    cx.notify();
                }),
            ))
    }
}

/// Builds a readable multi-line bank transfer card text.
pub(crate) fn format_bank_card_message(
    bank: &str,
    account: &str,
    owner: &str,
    amount: &str,
    memo: &str,
) -> String {
    let mut lines = vec![
        "Thông tin chuyển khoản".to_owned(),
        format!("• Ngân hàng: {}", bank.trim()),
        format!("• Số tài khoản: {}", account.trim()),
    ];
    if !owner.trim().is_empty() {
        lines.push(format!("• Chủ tài khoản: {}", owner.trim()));
    }
    if !amount.trim().is_empty() {
        lines.push(format!("• Số tiền: {}", amount.trim()));
    }
    if !memo.trim().is_empty() {
        lines.push(format!("• Nội dung: {}", memo.trim()));
    }
    lines.join("\n")
}

/// [`footer`] for dialogs built in other modules.
pub(crate) fn dialog_footer(
    cancel: &'static str,
    primary: &'static str,
    enabled: bool,
    on_cancel: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    on_ok: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    footer(cancel, primary, enabled, on_cancel, on_ok)
}

/// `.zl-modal__footer`: "Cancel" grey + primary blue, 16 / 500.
fn footer(
    cancel: &'static str,
    primary: &'static str,
    enabled: bool,
    on_cancel: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    on_ok: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let blue = token_rgb("--B55", "#0068ff");
    div()
        .flex()
        .flex_row()
        .justify_end()
        .gap(px(8.))
        .px(px(16.))
        .py(px(14.))
        .border_t_1()
        .border_color(crate::color::border_subtle())
        .child(
            div()
                .id("dialog-cancel")
                .px(px(16.))
                .h(px(40.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
                .text_color(rgb(token_rgb("--text-primary", "#081b3a")))
                .cursor_pointer()
                .text_size(px(16.))
                .font_family(FAMILY_SEMIBOLD)
                .on_click(on_cancel)
                .child(crate::i18n::t(cancel)),
        )
        .child(
            div()
                .id("dialog-ok")
                .px(px(16.))
                .h(px(40.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .bg(if enabled {
                    rgb(blue).into()
                } else {
                    crate::color::accent_disabled_bg()
                })
                .text_color(rgb(if enabled {
                    crate::color::on_accent_fg()
                } else {
                    crate::color::accent_disabled_fg()
                }))
                .text_size(px(16.))
                .font_family(FAMILY_SEMIBOLD)
                .when(enabled, |el| el.cursor_pointer().on_click(on_ok))
                .child(crate::i18n::t(primary)),
        )
}

pub(crate) fn modal_card(width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .rounded(px(8.))
        .bg(rgb(token_rgb("--layer-background", "#ffffff")))
        .border_1()
        .border_color(crate::color::border_subtle())
        .text_color(rgb(token_rgb("--text-primary", "#081b3a")))
        .shadow_lg()
        .flex()
        .flex_col()
}

fn chip_toggle(id: (&'static str, usize), label: &str, active: bool) -> gpui::Stateful<gpui::Div> {
    let blue = token_rgb("--B55", "#0068ff");
    let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
    let ink = token_rgb("--text-primary", "#081b3a");
    div()
        .id(id)
        .px(px(12.))
        .py(px(6.))
        .rounded(px(16.))
        .bg(rgb(if active { blue } else { subtle }))
        .text_color(rgb(if active {
            crate::color::on_accent_fg()
        } else {
            ink
        }))
        .text_size(px(13.))
        .cursor_pointer()
        .child(label.to_owned())
}

impl ShellView {
    /// Re-apply saved names: friend aliases, and group labels from the profile cache.
    ///
    /// A group row whose stored title is still `g_<id>` picks up the server name
    /// and writes that title back, so the next launch does not paint the id.
    pub(crate) fn apply_profile_aliases(&mut self) {
        let mut titles: Vec<(String, String)> = Vec::new();
        for c in &mut self.convs {
            if crate::shell::is_my_documents(&c.name, &c.thread_id, self.send2me_id.as_deref()) {
                if c.name != "My Documents" {
                    c.name = "My Documents".to_owned();
                    titles.push((c.thread_id.clone(), c.name.clone()));
                }
                continue;
            }
            if c.is_group {
                let profile_name = self
                    .profiles
                    .map
                    .get(&c.thread_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                let next =
                    crate::profiles::group_display_name(&c.thread_id, &c.name, &profile_name);
                if next != c.name {
                    c.name = next.clone();
                    if !crate::profiles::is_id_label(&c.thread_id, &next) {
                        titles.push((c.thread_id.clone(), next));
                    }
                }
                continue;
            }
            if let Some(alias) = self
                .profiles
                .aliases
                .get(&c.thread_id)
                .filter(|a| !a.is_empty())
            {
                c.name = alias.clone();
            } else if let Some(p) = self
                .profiles
                .map
                .get(&c.thread_id)
                .filter(|p| !p.name.is_empty())
            {
                c.name = p.name.clone();
            }
        }
        for g in &mut self.groups {
            let profile_name = self
                .profiles
                .map
                .get(&g.uid)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let next = crate::profiles::group_display_name(&g.uid, &g.name, &profile_name);
            if next != g.name {
                g.name = next.clone();
                if !crate::profiles::is_id_label(&g.uid, &next)
                    && !titles.iter().any(|(id, _)| id == &g.uid)
                {
                    titles.push((g.uid.clone(), next));
                }
            }
        }
        for (id, name) in titles {
            crate::data::persist_thread_title(&id, &name);
        }
        let fallback = crate::color::str_vi("STR_ZALO_USER", "Người dùng Zalo");
        let you = crate::color::str_vi("STR_YOU", "Bạn");
        let profiles = &self.profiles.map;
        let aliases = &self.profiles.aliases;
        let me_global_id = profiles
            .get(crate::profiles::ME)
            .map(|p| p.global_id.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let mut self_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for opt in [&self.uin, &self.send2me_id, &me_global_id] {
            if let Some(id) = opt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                self_ids.insert(id.to_owned());
            }
        }
        let mut rev_global: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (pc_id, p) in profiles {
            if !p.global_id.is_empty() && pc_id != crate::profiles::ME && !pc_id.starts_with("g_") {
                rev_global.insert(p.global_id.clone(), pc_id.clone());
            }
        }
        let conv_meta: std::collections::HashMap<u64, (String, String, bool)> = self
            .convs
            .iter()
            .map(|c| (c.id, (c.thread_id.clone(), c.name.clone(), c.is_group)))
            .collect();
        for (conv_id, msgs) in self.messages.iter_mut() {
            let meta = conv_meta.get(conv_id);
            for m in msgs.iter_mut() {
                if m.from_me {
                    continue;
                }
                if !m.sender_uid.is_empty() && self_ids.contains(m.sender_uid.trim()) {
                    m.from_me = true;
                    m.sender = you.to_owned();
                    m.sender_uid.clear();
                    continue;
                }
                if let Some(pc_uid) = rev_global.get(m.sender_uid.trim()) {
                    m.sender_uid = pc_uid.clone();
                }
                if let Some((tid, _, is_group)) = meta {
                    if !*is_group
                        && !tid.is_empty()
                        && (m.sender_uid.is_empty()
                            || (!profiles.contains_key(&m.sender_uid) && m.sender_uid != *tid))
                    {
                        m.sender_uid = tid.clone();
                    }
                }
                if m.sender_uid.is_empty() {
                    continue;
                }
                let raw = m.sender.trim();
                let unknown = raw.is_empty()
                    || raw == fallback
                    || crate::profiles::is_id_label(&m.sender_uid, raw);
                if !unknown {
                    continue;
                }
                if let Some(alias) = aliases.get(&m.sender_uid).filter(|a| !a.is_empty()) {
                    m.sender = alias.clone();
                    continue;
                }
                if let Some(name) = profiles
                    .get(&m.sender_uid)
                    .map(|p| p.name.trim().to_owned())
                    .filter(|n| !n.is_empty() && !crate::profiles::is_id_label(&m.sender_uid, n))
                {
                    m.sender = name;
                    continue;
                }
                if let Some((tid, conv_name, is_group)) = meta {
                    if !*is_group
                        && !conv_name.trim().is_empty()
                        && !crate::profiles::is_id_label(tid, conv_name)
                    {
                        m.sender = conv_name.clone();
                    }
                }
            }
        }
    }

    /// Open "Choose Paste Format" when clipboard holds both image and Excel/tabular text.
    pub(crate) fn open_paste_choice(&mut self, image: gpui::Image, text: String) {
        self.dialog = Some(Dialog::PasteChoice(PasteChoiceDlg { image, text }));
    }

    fn paste_choice_el(&self, p: &PasteChoiceDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = token_rgb("--text-secondary", "#5a6981");
        let snippet: String = p.text.chars().take(120).collect();
        let img_clone = p.image.clone();
        let text_clone = p.text.clone();
        modal_card(400.)
            .child(self.dialog_header("Chọn cách dán nội dung", cx))
            .child(
                div()
                    .px(px(16.))
                    .py(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .text_size(px(14.))
                    .child("Clipboard đang có cả hình ảnh và văn bản (Excel/bảng tính). Bạn muốn dán dưới dạng nào?")
                    .when(!snippet.is_empty(), |el| {
                        el.child(
                            div()
                                .p(px(10.))
                                .rounded(px(6.))
                                .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
                                .text_size(px(12.5))
                                .text_color(rgb(muted))
                                .child(snippet),
                        )
                    }),
            )
            .child(footer(
                "Dạng chữ",
                "Dạng hình ảnh",
                true,
                cx.listener(move |this, _, _, cx| {
                    let t = text_clone.clone();
                    this.dialog = None;
                    this.composer.update(cx, |i, cx| {
                        let mut cur = i.text();
                        cur.push_str(&t);
                        i.set_text(cur, cx);
                    });
                    cx.notify();
                }),
                cx.listener(move |this, _, _, cx| {
                    this.dialog = None;
                    this.paste_image(&img_clone);
                    cx.notify();
                }),
            ))
    }

    /// Open "Set Alias" for a friend conversation or contact row.
    pub(crate) fn open_alias(&mut self, uid: String, name: String, cx: &mut Context<Self>) {
        let initial = self
            .profiles
            .aliases
            .get(&uid)
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.clone());
        let input = cx.new(|cx| {
            let mut i = LineInput::new(cx, "Nhập tên gợi nhớ (để trống để bỏ)…");
            i.set_text(initial, cx);
            i
        });
        cx.subscribe(&input, |this, _, event: &LineEvent, cx| {
            if matches!(event, LineEvent::Submit(_)) {
                this.submit_alias(cx);
            }
        })
        .detach();
        self.dialog = Some(Dialog::Alias(AliasDlg {
            uid,
            name,
            input,
            busy: false,
        }));
    }

    fn submit_alias(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Alias(a)) = &self.dialog else {
            return;
        };
        if a.busy {
            return;
        }
        let uid = a.uid.clone();
        let raw_name = a.name.clone();
        let alias = a.input.read(cx).text().trim().to_owned();
        if alias.is_empty() {
            self.profiles.aliases.remove(&uid);
        } else {
            self.profiles.aliases.insert(uid.clone(), alias.clone());
        }
        if let Some(path) = crate::profiles::cache_path() {
            self.profiles.save(&path);
        }
        self.apply_profile_aliases();
        if let Some(f) = self.friends.iter_mut().find(|p| p.uid == uid) {
            f.name = self.profiles.alias_or(&uid, &raw_name).to_owned();
        }
        self.maybe_live(crate::bridge::LiveAction::UpdateAlias {
            uid,
            alias: alias.clone(),
        });
        self.dialog = None;
        self.set_note(if alias.is_empty() {
            "Đã xóa tên gợi nhớ".to_owned()
        } else {
            format!("Đã đặt tên gợi nhớ «{alias}»")
        });
        cx.notify();
    }

    fn alias_el(&self, a: &AliasDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = token_rgb("--text-secondary", "#5a6981");
        let person = Person {
            uid: a.uid.clone(),
            name: a.name.clone(),
            avatar: String::new(),
            note: String::new(),
        };
        modal_card(400.)
            .child(self.dialog_header("Đặt tên gợi nhớ", cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(14.))
                    .px(px(16.))
                    .py(px(20.))
                    .child(self.person_avatar(&person, 56.))
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(rgb(muted))
                            .text_center()
                            .child(format!(
                                "Tên gợi nhớ cho «{}» chỉ hiển thị với riêng bạn.",
                                a.name
                            )),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(40.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(rgb(token_rgb("--B55", "#0068ff")))
                            .text_size(px(15.))
                            .child(div().flex_1().child(a.input.clone())),
                    ),
            )
            .child(footer(
                "Hủy",
                "Lưu",
                !a.busy,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.submit_alias(cx);
                }),
            ))
    }

    /// Open "Report" modal from conversation context menu.
    pub(crate) fn open_report(&mut self, conv_id: u64, cx: &mut Context<Self>) {
        let Some(c) = self.conv(conv_id).cloned() else {
            return;
        };
        let input = cx.new(|cx| LineInput::new(cx, "Mô tả thêm lý do báo cáo (tuỳ chọn)…"));
        self.dialog = Some(Dialog::Report(ReportDlg {
            id: c.thread_id,
            name: c.name,
            is_group: c.is_group,
            reason: 0,
            input,
        }));
    }

    fn report_el(&self, r: &ReportDlg, cx: &mut Context<Self>) -> impl IntoElement {
        const REASONS: [&str; 5] = [
            "Spam / Tin nhắn rác",
            "Lừa đảo / Giả mạo",
            "Quấy rối / Làm phiền",
            "Nội dung nhạy cảm",
            "Lý do khác",
        ];
        modal_card(420.)
            .child(self.dialog_header("Báo xấu", cx))
            .child(
                div()
                    .px(px(16.))
                    .py(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .child(format!("Báo cáo vi phạm đối với «{}»:", r.name)),
                    )
                    .child(div().flex().flex_row().flex_wrap().gap(px(8.)).children(
                        REASONS.iter().enumerate().map(|(idx, &label)| {
                            chip_toggle(("rpt-reason", idx), label, r.reason == idx).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(Dialog::Report(dlg)) = &mut this.dialog {
                                        dlg.reason = idx;
                                    }
                                    cx.notify();
                                }),
                            )
                        }),
                    ))
                    .child(
                        div()
                            .h(px(40.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .child(div().flex_1().child(r.input.clone())),
                    ),
            )
            .child(footer(
                "Hủy",
                "Gửi báo cáo",
                true,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    let Some(Dialog::Report(dlg)) = &this.dialog else {
                        return;
                    };
                    let content = dlg.input.read(cx).text().trim().to_owned();
                    let content = if content.is_empty() {
                        REASONS
                            .get(dlg.reason)
                            .copied()
                            .unwrap_or("Spam")
                            .to_owned()
                    } else {
                        content
                    };
                    let action = crate::bridge::LiveAction::ReportAbuse {
                        target_id: dlg.id.clone(),
                        is_group: dlg.is_group,
                        reason: (dlg.reason + 1) as i64,
                        content,
                    };
                    this.maybe_live(action);
                    this.dialog = None;
                    this.set_note("Đã gửi báo xấu");
                    cx.notify();
                }),
            ))
    }

    /// Open "Broadcast Message" (batch message sender from Tools).
    pub(crate) fn open_broadcast(&mut self, cx: &mut Context<Self>) {
        self.rail_popup = None;
        let query = cx.new(|cx| LineInput::new(cx, "Tìm người nhận…"));
        let message = cx.new(|cx| LineInput::new(cx, "Nhập nội dung tin nhắn gửi đồng thời…"));
        cx.subscribe(&query, |this, _, event: &LineEvent, cx| {
            if let LineEvent::Change(q) = event {
                if let Some(Dialog::Broadcast(b)) = &mut this.dialog {
                    b.query_text = q.clone();
                }
                cx.notify();
            }
        })
        .detach();
        self.dialog = Some(Dialog::Broadcast(BroadcastDlg {
            query,
            query_text: String::new(),
            picked: Vec::new(),
            message,
        }));
    }

    fn broadcast_el(&self, b: &BroadcastDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let blue = token_rgb("--B55", "#0068ff");
        let q = b.query_text.trim().to_lowercase();
        let convs: Vec<_> = self
            .listed_convs()
            .into_iter()
            .filter(|c| q.is_empty() || c.name.to_lowercase().contains(&q))
            .collect();
        let picked: HashSet<u64> = b.picked.iter().copied().collect();
        let ready = !b.picked.is_empty() && !b.message.read(cx).text().trim().is_empty();
        modal_card(500.)
            .h(px(620.))
            .max_h(gpui::relative(1.))
            .child(self.dialog_header("Tin đồng thời", cx))
            .child(
                div().px(px(16.)).pt(px(12.)).child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .h(px(36.))
                        .px(px(12.))
                        .rounded(px(18.))
                        .border_1()
                        .border_color(crate::color::border_subtle())
                        .child(fonts::icon_el('\u{ED15}', "⌕", 16.))
                        .child(div().flex_1().text_size(px(14.)).child(b.query.clone())),
                ),
            )
            .child(
                div()
                    .px(px(16.))
                    .pt(px(8.))
                    .pb(px(4.))
                    .text_size(px(13.))
                    .text_color(rgb(token_rgb("--text-secondary", "#5a6981")))
                    .child(format!("Đã chọn {} người nhận", b.picked.len())),
            )
            .child(
                div()
                    .id("broadcast-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(px(8.))
                    .children(convs.into_iter().map(|c| {
                        let on = picked.contains(&c.id);
                        let id = c.id;
                        let person = Person {
                            uid: c.thread_id.clone(),
                            name: c.name.clone(),
                            avatar: String::new(),
                            note: String::new(),
                        };
                        div()
                            .id(("bc-row", id))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.))
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|st| st.bg(rgb(hover)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(Dialog::Broadcast(dlg)) = &mut this.dialog {
                                    if let Some(pos) = dlg.picked.iter().position(|&x| x == id) {
                                        dlg.picked.remove(pos);
                                    } else {
                                        dlg.picked.push(id);
                                    }
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .size(px(18.))
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(rgb(if on { blue } else { ink }))
                                    .when(on, |el| el.bg(rgb(blue)))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(rgb(crate::color::on_accent_fg()))
                                    .text_size(px(11.))
                                    .child(if on { "✓" } else { "" }),
                            )
                            .child(self.person_avatar(&person, 32.))
                            .child(div().flex_1().truncate().text_size(px(14.)).child(c.name))
                    })),
            )
            .child(
                div()
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(crate::color::border_subtle())
                    .child(
                        div()
                            .h(px(40.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .bg(rgb(token_rgb("--input-field-bg-filled", "#f1f2f4")))
                            .child(div().flex_1().child(b.message.clone())),
                    ),
            )
            .child(footer(
                "Hủy",
                "Gửi đồng thời",
                ready,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    let Some(Dialog::Broadcast(dlg)) = this.dialog.take() else {
                        return;
                    };
                    let text = dlg.message.read(cx).text().trim().to_owned();
                    if text.is_empty() || dlg.picked.is_empty() {
                        return;
                    }
                    let back = this.selected;
                    let mut sent = 0usize;
                    for id in dlg.picked {
                        if this.conv(id).is_some() {
                            this.select_conv(id);
                            this.send_draft(&text, cx);
                            sent += 1;
                        }
                    }
                    if let Some(id) = back {
                        this.select_conv(id);
                    }
                    this.set_note(format!("Đã gửi tin đồng thời tới {sent} hội thoại"));
                    cx.notify();
                }),
            ))
    }

    /// Open "Remove from Friends" confirmation modal.
    pub(crate) fn open_remove_friend(&mut self, uid: String, name: String) {
        self.dialog = Some(Dialog::RemoveFriend(RemoveFriendDlg { uid, name }));
    }

    fn remove_friend_el(&self, f: &RemoveFriendDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let uid = f.uid.clone();
        let name = f.name.clone();
        modal_card(400.)
            .child(self.dialog_header("Xóa bạn", cx))
            .child(
                div()
                    .px(px(16.))
                    .py(px(20.))
                    .text_size(px(14.))
                    .child(format!(
                        "Bạn có chắc chắn muốn xóa «{name}» khỏi danh sách bạn bè?"
                    )),
            )
            .child(footer(
                "Hủy",
                "Xóa bạn",
                true,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(move |this, _, _, cx| {
                    this.dialog = None;
                    this.friends.retain(|p| p.uid != uid);
                    this.maybe_live(crate::bridge::LiveAction::RemoveFriend { uid: uid.clone() });
                    this.set_note(format!("Đã xóa «{name}» khỏi danh sách bạn bè"));
                    cx.notify();
                }),
            ))
    }

    /// Opens "Two-Factor Authentication" modal and sends initial `/api/update/setting/2FA` (`submitOTP=0`).
    pub(crate) fn open_two_factor_dialog(&mut self, turn_on: bool, cx: &mut Context<Self>) {
        if !self.has_session || cfg!(test) {
            self.set_note("Cần đăng nhập để bật/tắt bảo mật 2 lớp");
            return;
        }
        let otp = cx.new(|cx| LineInput::new(cx, "Nhập mã OTP"));
        cx.subscribe(&otp, |this, _, event, cx| match event {
            LineEvent::Change(text) => {
                if let Some(Dialog::TwoFactorOtp(d)) = this.dialog.as_mut() {
                    d.otp_text = text.clone();
                    cx.notify();
                }
            }
            LineEvent::Submit(text) => {
                let code = text.trim().to_owned();
                if !code.is_empty() {
                    this.submit_two_factor_otp(Some(code), cx);
                }
            }
            LineEvent::PasteImage(_) | LineEvent::PasteWithChoice { .. } => {}
        })
        .detach();
        self.dialog = Some(Dialog::TwoFactorOtp(TwoFactorOtpDlg {
            turn_on,
            otp,
            otp_text: String::new(),
            busy: true,
            status: Some("Đang gửi yêu cầu xác thực tới Zalo…".into()),
            error: None,
        }));
        self.submit_two_factor_otp(None, cx);
    }

    fn submit_two_factor_otp(&mut self, otp: Option<String>, cx: &mut Context<Self>) {
        let Some(turn_on) = self.dialog.as_ref().and_then(|dlg| match dlg {
            Dialog::TwoFactorOtp(d) => Some(d.turn_on),
            _ => None,
        }) else {
            return;
        };
        if cfg!(test) || !self.has_session {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if let Some(Dialog::TwoFactorOtp(d)) = self.dialog.as_mut() {
            d.busy = true;
            d.error = None;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.update_tfa_settings(turn_on, otp.as_deref())
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match res {
                    Ok(()) => {
                        view.privacy
                            .values
                            .get_or_insert_with(std::collections::HashMap::new)
                            .insert("setting_2FA_status".to_owned(), i64::from(turn_on));
                        if matches!(view.dialog, Some(Dialog::TwoFactorOtp(_))) {
                            view.dialog = None;
                        }
                        view.set_note(if turn_on {
                            "Đã bật bảo mật 2 lớp"
                        } else {
                            "Đã tắt bảo mật 2 lớp"
                        });
                    }
                    Err(err) => {
                        if let Some(Dialog::TwoFactorOtp(d)) = view.dialog.as_mut() {
                            d.busy = false;
                            d.status = Some(
                                "Nhập mã OTP gửi về điện thoại hoặc thiết bị di động rồi bấm Xác nhận.".into(),
                            );
                            d.error = Some(err);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn two_factor_el(&self, t: &TwoFactorOtpDlg, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let danger = token_rgb("--R60", "#c31818");
        let ok = !t.busy && !t.otp_text.trim().is_empty();
        let otp_submit = t.otp_text.trim().to_owned();
        let title = if t.turn_on {
            "Bật bảo mật 2 lớp"
        } else {
            "Tắt bảo mật 2 lớp"
        };
        modal_card(420.)
            .child(self.dialog_header(title, cx))
            .child(
                div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .when_some(t.status.clone(), |el, st| {
                        el.child(div().text_size(px(13.)).text_color(rgb(muted)).child(st))
                    })
                    .child(
                        div()
                            .h(px(38.))
                            .px(px(12.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(crate::color::border_subtle())
                            .flex()
                            .items_center()
                            .text_size(px(14.))
                            .text_color(rgb(ink))
                            .child(t.otp.clone()),
                    )
                    .when(!t.busy, |el| {
                        el.child(
                            div()
                                .id("tfa-resend-otp")
                                .mr_auto()
                                .cursor_pointer()
                                .text_size(px(12.5))
                                .text_color(rgb(blue))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.submit_two_factor_otp(None, cx);
                                }))
                                .child("Gửi lại yêu cầu OTP"),
                        )
                    })
                    .when_some(t.error.clone(), |el, err| {
                        el.child(div().text_size(px(12.5)).text_color(rgb(danger)).child(err))
                    }),
            )
            .child(footer(
                "Hủy",
                if t.busy {
                    "Đang xử lý…"
                } else {
                    "Xác nhận OTP"
                },
                ok,
                cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(move |this, _, _, cx| {
                    this.submit_two_factor_otp(Some(otp_submit.clone()), cx);
                }),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_found_user_and_recomms() {
        let v = serde_json::json!({"data": {"uid": "42", "display_name": "Lan", "avatar": "https://a/b.jpg"}});
        let p = found_user_from(&v).unwrap();
        assert_eq!((p.uid.as_str(), p.name.as_str()), ("42", "Lan"));
        assert!(found_user_from(&serde_json::json!({"data": {"uid": "0"}})).is_none());
        let r = serde_json::json!({"data": {"recommItems": [{"dataInfo": {"userId": "7", "displayName": "Chi", "recommInfo": {"message": ""}}}]}});
        let list = recomms_from(&r);
        assert_eq!(list[0].note, "Từ gợi ý kết bạn");
    }

    #[test]
    fn formats_bank_card_transfer_summary() {
        let msg = format_bank_card_message(
            "Vietcombank",
            "0123456789",
            "NGUYEN VAN A",
            "200.000đ",
            "An trua",
        );
        assert!(msg.contains("Ngân hàng: Vietcombank"));
        assert!(msg.contains("Số tài khoản: 0123456789"));
        assert!(msg.contains("Chủ tài khoản: NGUYEN VAN A"));
        assert!(msg.contains("Số tiền: 200.000đ"));
        assert!(msg.contains("Nội dung: An trua"));
    }
}
