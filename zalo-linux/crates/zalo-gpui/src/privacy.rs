//! "Hide Conversation" with a PIN and "Lock Zalo Screen", ported from
//! zalo-gui (`lock.rs`).
//!
//! - Hidden chats live on the account (`/api/hiddenconvers/*`): the server
//!   keeps the md5 of the 4-digit PIN, so checking it needs no round trip.
//!   Hidden rows stay out of the list; searching the exact name shows them,
//!   opening one asks for the PIN, which unlocks hidden chats for this run.
//! - The screen lock is local like Zalo PC: `salt$md5(salt + code)` in
//!   `gpui-prefs.json` (`lockHash`). Ctrl+L locks; a set code also locks at
//!   start-up.

use std::collections::HashSet;

use gpui::{
    div, prelude::*, px, rgb, ClickEvent, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};
use md5::{Digest, Md5};
use serde_json::Value;

use crate::color::token_rgb;
use crate::dialogs::{Dialog, Person};
use crate::fonts::FAMILY_SEMIBOLD;
use crate::line_input::{LineEvent, LineInput};
use crate::shell::ShellView;

fn md5_hex(bytes: &[u8]) -> String {
    format!("{:x}", Md5::digest(bytes))
}

/// `salt$md5(salt + code)` (same format as zalo-gui).
#[must_use]
pub fn hash_code(code: &str, salt: &str) -> String {
    format!("{salt}${}", md5_hex(format!("{salt}{code}").as_bytes()))
}

/// True when `code` matches a stored `salt$hash`.
#[must_use]
pub fn code_matches(stored: &str, code: &str) -> bool {
    stored
        .split_once('$')
        .is_some_and(|(salt, _)| hash_code(code, salt) == stored)
}

fn new_hash(code: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    hash_code(
        code,
        &format!("{:x}", nanos ^ u128::from(std::process::id())),
    )
}

/// `{pin, threads}` of `/api/hiddenconvers/get-all` → (pin md5, thread ids).
#[must_use]
pub fn hidden_from(value: &Value) -> (String, HashSet<String>) {
    let root = value.get("data").unwrap_or(value);
    let pin = root
        .get("pin")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let threads = root
        .get("threads")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| {
                    let id = match item.get("thread_id")? {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        _ => return None,
                    };
                    let group = match item.get("is_group") {
                        Some(Value::Bool(b)) => *b,
                        Some(Value::Number(n)) => n.as_i64() == Some(1),
                        _ => false,
                    };
                    Some(if group {
                        format!("g_{}", id.trim_start_matches("g_"))
                    } else {
                        id
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (pin, threads)
}

/// Server entry for one of our thread ids.
#[must_use]
pub fn hidden_item(thread_id: &str) -> Value {
    match thread_id.strip_prefix("g_") {
        Some(group) => serde_json::json!({ "thread_id": group, "is_group": 1 }),
        None => serde_json::json!({ "thread_id": thread_id, "is_group": 0 }),
    }
}

/// Account-side hidden chats.
#[derive(Clone, Debug, Default)]
pub struct Hidden {
    /// md5 of the PIN, empty when none is set.
    pub pin_md5: String,
    /// Server reply arrived.
    pub loaded: bool,
    /// PIN entered this run: hidden chats show in the list.
    pub unlocked: bool,
}

/// What the PIN / code dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinMode {
    /// First hide: choose a 4-digit PIN, then hide `conv`.
    SetPin { conv: Option<u64> },
    /// Open a hidden chat (`conv`) or show all hidden chats.
    Unlock { conv: Option<u64> },
    /// Create or change the screen-lock code.
    LockSet,
    /// Turn the screen lock off (needs the current code).
    LockOff,
}

/// PIN / passcode dialog.
pub struct PinDialog {
    mode: PinMode,
    first: Entity<LineInput>,
    second: Entity<LineInput>,
    error: Option<String>,
    busy: bool,
}

impl ShellView {
    /// Fetch hidden chats once per session (and after changes).
    pub(crate) fn load_hidden(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.get_hidden_chats()
                            .await
                            .map(|v| hidden_from(&v))
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Ok((pin, threads)) = got {
                    view.apply_hidden(pin, &threads);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_hidden(&mut self, pin: String, threads: &HashSet<String>) {
        self.hidden_srv.pin_md5 = pin;
        self.hidden_srv.loaded = true;
        // The account list replaces the old local-only set.
        self.hidden = self
            .convs
            .iter()
            .filter(|c| threads.contains(&c.thread_id))
            .map(|c| c.id)
            .collect();
        if !self.hidden_srv.unlocked {
            if let Some(id) = self.selected.filter(|id| self.hidden.contains(id)) {
                let _ = id;
                self.selected = None;
                self.info_open = false;
            }
        }
    }

    /// True when `id` must stay out of the list right now.
    pub(crate) fn conv_hidden(&self, id: u64) -> bool {
        self.hidden.contains(&id) && !self.hidden_srv.unlocked
    }

    /// True when `raw_query` matches the account's hidden-chat PIN.
    pub(crate) fn pin_matches(&self, raw_query: &str) -> bool {
        let pin = raw_query.trim();
        !self.hidden.is_empty()
            && !self.hidden_srv.pin_md5.is_empty()
            && !pin.is_empty()
            && md5_hex(pin.as_bytes()) == self.hidden_srv.pin_md5
    }

    /// Re-hide unlocked hidden conversations in this session.
    pub(crate) fn lock_hidden_chats(&mut self) {
        self.hidden_srv.unlocked = false;
        if let Some(id) = self.selected.filter(|id| self.hidden.contains(id)) {
            let _ = id;
            self.selected = None;
            self.info_open = false;
        }
        self.set_note("Đã ẩn lại các cuộc trò chuyện ẩn");
    }

    /// Menu / info "Hide Conversation": needs a PIN on the account first.
    pub(crate) fn hide_conv(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.has_session {
            self.hidden.insert(id);
            self.save_conv_flags();
            let name = self.conv_display_name(id);
            self.dialog = Some(Dialog::HiddenGuide(name));
            self.set_note("Chưa đăng nhập — chỉ ẩn trên máy này");
            cx.notify();
            return;
        }
        if self.hidden_srv.pin_md5.is_empty() {
            self.open_pin(PinMode::SetPin { conv: Some(id) }, cx);
            return;
        }
        self.write_hidden(id, true, cx);
    }

    /// "Unhide Conversation".
    pub(crate) fn unhide_conv(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.has_session {
            self.hidden.remove(&id);
            self.save_conv_flags();
            return;
        }
        self.write_hidden(id, false, cx);
    }

    fn write_hidden(&mut self, id: u64, hide: bool, cx: &mut Context<Self>) {
        let Some(conv) = self.conv(id).cloned() else {
            return;
        };
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if hide {
            self.hidden.insert(id);
            if self.selected == Some(id) && !self.hidden_srv.unlocked {
                self.selected = None;
                self.info_open = false;
            }
        } else {
            self.hidden.remove(&id);
        }
        let item = hidden_item(&conv.thread_id);
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let (add, del) = if hide {
                            (vec![item], Vec::new())
                        } else {
                            (Vec::new(), vec![item])
                        };
                        api.update_hidden_chats(&add, &del)
                            .await
                            .map_err(|e| e.to_string())?;
                        api.get_hidden_chats()
                            .await
                            .map(|v| hidden_from(&v))
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match got {
                    Ok((pin, threads)) => {
                        view.apply_hidden(pin, &threads);
                        if hide {
                            let name = view.conv_display_name(id);
                            view.dialog = Some(Dialog::HiddenGuide(name));
                            view.set_note(
                                "Đã ẩn — tìm đúng tên, nhập PIN vào Tìm kiếm hoặc bấm «•••» để mở lại",
                            );
                        } else {
                            view.set_note("Đã bỏ ẩn trò chuyện");
                        }
                    }
                    Err(e) => view.set_note(format!("Không cập nhật được: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the PIN / code dialog.
    pub(crate) fn open_pin(&mut self, mode: PinMode, cx: &mut Context<Self>) {
        let (a, b) = match mode {
            PinMode::SetPin { .. } => ("Nhập mã PIN (4 chữ số)", "Nhập lại mã PIN"),
            PinMode::Unlock { .. } => ("Nhập mã PIN", ""),
            PinMode::LockSet => ("Mã khóa mới (ít nhất 4 ký tự)", "Nhập lại mã khóa"),
            PinMode::LockOff => ("Mã khóa hiện tại", ""),
        };
        let first = cx.new(|cx| LineInput::new_masked(cx, a));
        let second = cx.new(|cx| LineInput::new_masked(cx, b));
        for input in [&first, &second] {
            cx.subscribe(input, |this, _, e: &LineEvent, cx| {
                if matches!(e, LineEvent::Submit(_)) {
                    this.submit_pin(cx);
                    cx.notify();
                }
            })
            .detach();
        }
        self.menu = None;
        self.dialog = Some(Dialog::Pin(PinDialog {
            mode,
            first,
            second,
            error: None,
            busy: false,
        }));
    }

    fn submit_pin(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Pin(d)) = self.dialog.as_ref() else {
            return;
        };
        if d.busy {
            return;
        }
        let a = d.first.read(cx).text();
        let b = d.second.read(cx).text();
        let mode = d.mode;
        let error = match mode {
            PinMode::SetPin { .. } if a.len() != 4 || !a.chars().all(|c| c.is_ascii_digit()) => {
                Some("Mã PIN gồm đúng 4 chữ số")
            }
            PinMode::SetPin { .. } | PinMode::LockSet if a != b => Some("Hai lần nhập không khớp"),
            PinMode::LockSet if a.chars().count() < 4 => Some("Mã khóa cần ít nhất 4 ký tự"),
            PinMode::Unlock { .. } if md5_hex(a.as_bytes()) != self.hidden_srv.pin_md5 => {
                Some("Mã PIN không đúng")
            }
            PinMode::LockOff if !code_matches(&self.prefs.str("lockHash", ""), &a) => {
                Some("Mã khóa không đúng")
            }
            _ => None,
        };
        if let Some(e) = error {
            if let Some(Dialog::Pin(d)) = self.dialog.as_mut() {
                d.error = Some(e.to_owned());
            }
            return;
        }
        match mode {
            PinMode::Unlock { conv } => {
                self.dialog = None;
                self.hidden_srv.unlocked = true;
                self.set_note("Đã mở các trò chuyện ẩn cho lần dùng này");
                if let Some(id) = conv {
                    self.select_conv(id);
                }
            }
            PinMode::LockSet => {
                self.dialog = None;
                self.prefs.set("lockHash", Value::String(new_hash(&a)));
                self.set_note("Đã đặt mã khóa — nhấn Ctrl + L để khóa Zalo");
            }
            PinMode::LockOff => {
                self.dialog = None;
                self.prefs.set("lockHash", Value::String(String::new()));
                self.set_note("Đã tắt khóa màn hình");
            }
            PinMode::SetPin { conv } => {
                let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
                    return;
                };
                if let Some(Dialog::Pin(d)) = self.dialog.as_mut() {
                    d.busy = true;
                }
                cx.spawn(async move |this, cx| {
                    let got = cx
                        .background_executor()
                        .spawn(async move {
                            crate::bridge::block_api(&dir, |api| async move {
                                api.update_hidden_pin(&a).await.map_err(|e| e.to_string())?;
                                api.get_hidden_chats()
                                    .await
                                    .map(|v| hidden_from(&v))
                                    .map_err(|e| e.to_string())
                            })
                        })
                        .await;
                    let _ = this.update(cx, |view, cx| {
                        match got {
                            Ok((pin, threads)) => {
                                view.dialog = None;
                                view.apply_hidden(pin, &threads);
                                if let Some(id) = conv {
                                    view.write_hidden(id, true, cx);
                                } else {
                                    view.set_note("Đã đặt mã PIN cho trò chuyện ẩn");
                                }
                            }
                            Err(e) => {
                                if let Some(Dialog::Pin(d)) = view.dialog.as_mut() {
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
        }
    }

    pub(crate) fn pin_el(&self, d: &PinDialog, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let (title, desc, two) = match d.mode {
            PinMode::SetPin { .. } => (
                "Thiết lập mã PIN",
                "Mã PIN dùng để mở các trò chuyện bị ẩn. Hãy ghi nhớ mã này, Zalo không thể khôi phục giúp bạn.",
                true,
            ),
            PinMode::Unlock { .. } => ("Nhập mã PIN", "Nhập mã PIN để xem trò chuyện đã ẩn.", false),
            PinMode::LockSet => (
                "Tạo mã khóa màn hình",
                "Mã khóa lưu trên máy này. Nhấn Ctrl + L để khóa Zalo khi bạn rời máy.",
                true,
            ),
            PinMode::LockOff => ("Tắt khóa màn hình", "Nhập mã khóa hiện tại để tắt.", false),
        };
        let field = |input: Entity<LineInput>| {
            div()
                .w_full()
                .h(px(40.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .border_1()
                .border_color(crate::color::border_subtle())
                .text_size(px(16.))
                .child(div().flex_1().child(input))
        };
        div()
            .w(px(400.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(ink))
            .child(self.dialog_title_bar(title, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .p(px(16.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(rgb(muted))
                            .child(crate::media::wrap_bubble_text(desc, 390.)),
                    )
                    .child(field(d.first.clone()))
                    .when(two, |el| el.child(field(d.second.clone())))
                    .when_some(d.error.clone(), |el, e| {
                        el.child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(token_rgb("--R60", "#c31818")))
                                .child(e),
                        )
                    }),
            )
            .child(crate::dialogs::dialog_footer(
                "Hủy",
                if d.busy {
                    "Đang lưu…"
                } else {
                    "Xác nhận"
                },
                !d.busy,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }),
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.submit_pin(cx);
                    cx.notify();
                }),
            ))
    }

    /// Ctrl+L / start-up: cover the window until the code is entered.
    pub(crate) fn lock_now(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.prefs.str("lockHash", "").is_empty() {
            self.settings_open = true;
            self.rail = crate::rail::Rail::Settings;
            self.settings_page = crate::settings::SettingsPage::Security;
            self.set_note("Tạo mã khóa trước khi khóa Zalo");
            return;
        }
        self.menu = None;
        self.dialog = None;
        self.viewer = None;
        self.chat_sel.clear();
        self.close_settings();
        self.lock_input.update(cx, |i, cx| i.clear(cx));
        self.lock_error = false;
        self.locked = true;
        let handle = gpui::Focusable::focus_handle(self.lock_input.read(cx), cx);
        window.focus(&handle);
    }

    fn try_unlock(&mut self, cx: &mut Context<Self>) {
        let code = self.lock_input.read(cx).text();
        if code_matches(&self.prefs.str("lockHash", ""), &code) {
            self.locked = false;
            self.lock_error = false;
        } else {
            self.lock_error = true;
            self.lock_input.update(cx, |i, cx| i.clear(cx));
        }
    }

    /// Full-window lock layer (avatar, name, code field).
    pub(crate) fn lock_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let me = self
            .profiles
            .map
            .get(crate::profiles::ME)
            .cloned()
            .unwrap_or_default();
        let person = Person {
            uid: crate::profiles::ME.into(),
            name: if me.name.is_empty() {
                "Zalo".into()
            } else {
                me.name.clone()
            },
            avatar: String::new(),
            note: String::new(),
        };
        div()
            .id("lock-layer")
            .absolute()
            // Below the caption bar: minimize / close keep working.
            .top(px(zalo_design::LINUX_TITLEBAR_H))
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(360.))
                    .p(px(32.))
                    .rounded(px(12.))
                    .bg(rgb(crate::color::token_rgb(
                        "--layer-background",
                        "#ffffff",
                    )))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(14.))
                    .text_color(rgb(ink))
                    .child(self.dialog_avatar(&person, 72.))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_family(FAMILY_SEMIBOLD)
                            .child(person.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(rgb(muted))
                            .child("Zalo đang khóa. Nhập mã khóa để mở."),
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
                            .border_color(if self.lock_error {
                                rgb(token_rgb("--R60", "#c31818"))
                            } else {
                                rgb(blue)
                            })
                            .text_size(px(16.))
                            .child(div().flex_1().child(self.lock_input.clone())),
                    )
                    .when(self.lock_error, |el| {
                        el.child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(token_rgb("--R60", "#c31818")))
                                .child("Mã khóa không đúng"),
                        )
                    })
                    .child(
                        div()
                            .id("lock-open")
                            .w_full()
                            .h(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .bg(rgb(blue))
                            .text_color(rgb(0xffffff))
                            .font_family(FAMILY_SEMIBOLD)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.try_unlock(cx);
                                cx.notify();
                            }))
                            .child("Mở khóa"),
                    ),
            )
    }

    /// Display name for a conversation row (falls back to empty when missing).
    fn conv_display_name(&self, id: u64) -> String {
        self.convs
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }

    /// Guide modal shown after hiding a conversation ("Conversation hidden successfully").
    pub(crate) fn hidden_guide_el(
        &self,
        conv_name: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let blue_hover = token_rgb("--B60", "#005ae0");
        let (p1, p2) = hidden_guide_paragraphs(conv_name);
        crate::dialogs::modal_card(416.)
            .child(self.dialog_title_bar(
                crate::color::str_vi("STR_TITLE_POPUP_HIDDEN_4", "Ẩn trò chuyện thành công"),
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
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(rgb(ink))
                            .child(styled_guide_paragraph(&p1, ink)),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(rgb(muted))
                            .child("Hướng dẫn:"),
                    )
                    .child(hidden_guide_preview())
                    .child(
                        div()
                            .w_full()
                            .whitespace_normal()
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(rgb(ink))
                            .child(styled_guide_paragraph(&p2, ink)),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .px(px(16.))
                    .pb(px(16.))
                    .child(
                        div()
                            .id("hidden-guide-close")
                            .px(px(20.))
                            .h(px(36.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .bg(rgb(blue))
                            .hover(|st| st.bg(rgb(blue_hover)))
                            .text_color(rgb(0xff_ff_ff))
                            .text_size(px(14.))
                            .font_family(FAMILY_SEMIBOLD)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialog = None;
                                cx.notify();
                            }))
                            .child(crate::i18n::t("Đóng")),
                    ),
            )
    }

    /// Subscribe the lock field once (Enter unlocks).
    pub(crate) fn wire_lock_input(&mut self, cx: &mut Context<Self>) {
        cx.subscribe(&self.lock_input, |this, _, e: &LineEvent, cx| {
            if matches!(e, LineEvent::Submit(_)) {
                this.try_unlock(cx);
                cx.notify();
            }
        })
        .detach();
    }
}

/// Bold-annotated paragraph segments (`(slice, is_bold)`).
pub(crate) type GuideParagraph = Vec<(String, bool)>;

/// Segmented `(text, bold)` pairs for the "Conversation hidden successfully" guide modal.
pub(crate) fn hidden_guide_paragraphs(conv_name: &str) -> (GuideParagraph, GuideParagraph) {
    let target = conv_name.trim();
    let p1 = if target.is_empty() {
        vec![
            (
                "Bạn đã ẩn trò chuyện thành công. Để mở các trò chuyện đã ẩn, bạn có thể nhập "
                    .into(),
                false,
            ),
            ("mã PIN đã cài đặt hoặc tên trò chuyện ẩn".into(), true),
            (" tại khung tìm kiếm.".into(), false),
        ]
    } else {
        vec![
            ("Bạn đã ẩn trò chuyện ".into(), false),
            (target.to_owned(), true),
            (
                " thành công. Để mở các trò chuyện đã ẩn, bạn có thể nhập ".into(),
                false,
            ),
            ("mã PIN đã cài đặt hoặc tên trò chuyện ẩn".into(), true),
            (" tại khung tìm kiếm.".into(), false),
        ]
    };
    let p2 = vec![
        ("Ngoài ra bạn có thể vào ".into(), false),
        (
            "Cài đặt > Tin nhắn > Thiết lập ẩn trò chuyện".into(),
            true,
        ),
        (" để thay đổi hoặc đặt lại mã PIN.".into(), false),
    ];
    (p1, p2)
}

fn styled_guide_paragraph(segments: &[(String, bool)], ink: u32) -> gpui::StyledText {
    let total_len: usize = segments.iter().map(|(s, _)| s.len()).sum();
    let mut text = String::with_capacity(total_len);
    let mut runs = Vec::with_capacity(segments.len());
    let color: gpui::Hsla = rgb(ink).into();
    let use_semibold = crate::fonts::report().semibold;
    for (part, bold) in segments {
        if part.is_empty() {
            continue;
        }
        text.push_str(part);
        let mut font = if *bold && use_semibold {
            gpui::font(FAMILY_SEMIBOLD)
        } else {
            gpui::font(crate::fonts::FAMILY_UI)
        };
        if *bold {
            font.weight = gpui::FontWeight::SEMIBOLD;
        }
        runs.push(gpui::TextRun {
            len: part.len(),
            font,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    gpui::StyledText::new(text).with_runs(runs)
}

/// Miniature search-bar card matching Zalo PC's hidden-chat guide illustration.
fn hidden_guide_preview() -> impl IntoElement {
    let paper = crate::color::token_rgb("--layer-background", "#ffffff");
    let field = crate::color::token_rgb("--input-field-bg-filled", "#e8eaef");
    let ink = crate::color::token_rgb("--text-primary", "#142646");
    let muted = crate::color::token_rgb("--text-secondary", "#758297");
    div()
        .w_full()
        .rounded(px(4.))
        .border_1()
        .border_color(crate::color::border_subtle())
        .bg(rgb(paper))
        .px(px(16.))
        .py(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w_full()
                .max_w(px(332.))
                .h(px(36.))
                .px(px(10.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .h(px(30.))
                        .px(px(12.))
                        .rounded(px(15.))
                        .bg(rgb(field))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(muted))
                                .child(crate::fonts::icon_el(
                                    crate::fonts::ic::SEARCH_FIELD,
                                    "⌕",
                                    13.,
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(rgb(muted))
                                .truncate()
                                .child("Tìm bạn bè, nhóm và tin nhắn"),
                        ),
                )
                .child(
                    div()
                        .size(px(26.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(ink))
                        .child(crate::fonts::icon_el(
                            crate::fonts::ic::ADD_CONTACT,
                            "+",
                            18.,
                        )),
                )
                .child(
                    div()
                        .size(px(26.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(ink))
                        .child(crate::fonts::icon_el(
                            crate::fonts::ic::GROUP_ADD,
                            "☰",
                            18.,
                        )),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_hidden_reply() {
        let stored = hash_code("2468", "abc");
        assert!(code_matches(&stored, "2468"));
        assert!(!code_matches(&stored, "1357"));
        let (pin, threads) = hidden_from(&serde_json::json!({"data": {"pin": "x", "threads": [
            {"thread_id": "123", "is_group": 1}, {"thread_id": 45, "is_group": 0}
        ]}}));
        assert_eq!(pin, "x");
        assert!(threads.contains("g_123") && threads.contains("45"));
        assert_eq!(hidden_item("g_9")["is_group"], 1);
        assert_eq!(md5_hex(b"1234"), "81dc9bdb52d04dc20036dbd8313ed055");
    }

    #[test]
    fn hidden_guide_copy_highlights_name_and_paths() {
        let (p1, p2) = hidden_guide_paragraphs("  Alice Nguyen  ");
        let full1: String = p1.iter().map(|(s, _)| s.as_str()).collect();
        let full2: String = p2.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            full1,
            "Bạn đã ẩn trò chuyện Alice Nguyen thành công. Để mở các trò chuyện đã ẩn, bạn có thể nhập mã PIN đã cài đặt hoặc tên trò chuyện ẩn tại khung tìm kiếm."
        );
        assert_eq!(
            full2,
            "Ngoài ra bạn có thể vào Cài đặt > Tin nhắn > Thiết lập ẩn trò chuyện để thay đổi hoặc đặt lại mã PIN."
        );
        assert!(p1
            .iter()
            .any(|(s, bold)| s == "Alice Nguyen" && *bold));
        assert!(p1
            .iter()
            .any(|(s, bold)| s == "mã PIN đã cài đặt hoặc tên trò chuyện ẩn" && *bold));
        assert!(p2
            .iter()
            .any(|(s, bold)| s == "Cài đặt > Tin nhắn > Thiết lập ẩn trò chuyện" && *bold));

        let (empty_p1, _) = hidden_guide_paragraphs("   ");
        let empty_full: String = empty_p1.iter().map(|(s, _)| s.as_str()).collect();
        assert!(empty_full.starts_with("Bạn đã ẩn trò chuyện thành công."));
    }
}
