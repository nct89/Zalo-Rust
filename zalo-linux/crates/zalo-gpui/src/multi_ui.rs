//! ZaloMulti multi-account panel anchored to the left sidebar (`RailPopup::MultiAccount`).
//!
//! Powered by `zalo-multi`: authenticates against `https://api.lza.lol/zalomulti`
//! (`zms2` Ed25519 sessions + 15-day machine-sealed refresh tokens), enforces
//! server-signed clone quotas, and launches/focuses/stops isolated native
//! `zalo-gpui --profile <name>` instances.

use gpui::{
    div, hsla, prelude::*, px, rgb, ClickEvent, Context, Entity, Focusable, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement, Styled,
};

use crate::color::token_rgb;
use crate::fonts::{
    fa_icon, FAMILY_BOLD, FAMILY_SEMIBOLD, FA_CART_SHOPPING, FA_CHECK, FA_CROWN, FA_EXTERNAL_LINK,
    FA_FOLDER_OPEN, FA_GEAR, FA_GLOBE, FA_PLAY, FA_PLUS, FA_RIGHT_FROM_BRACKET, FA_ROTATE, FA_STOP,
    FA_TRASH, FA_XMARK,
};
use crate::line_input::{LineEvent, LineInput};
use crate::shell::ShellView;
use crate::widgets::tip;

/// Snapshot of one clone profile for UI rendering.
#[derive(Clone, Debug)]
pub(crate) struct CloneRow {
    pub name: String,
    pub running: bool,
    pub zalo_name: Option<String>,
    pub proxy: String,
}

/// Active inline form inside the ZaloMulti popover.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum MultiEditMode {
    #[default]
    None,
    Add,
    Rename(String),
    Proxy(String),
    ConfirmDelete(String),
}

/// Persistent UI state for `RailPopup::MultiAccount`.
pub(crate) struct MultiAccountState {
    pub status: zalo_multi::AccountStatus,
    pub clones: Vec<CloneRow>,
    pub busy: bool,
    pub error: Option<String>,
    pub info: Option<String>,
    pub remember: bool,
    pub edit_mode: MultiEditMode,
    pub email_input: Entity<LineInput>,
    pub password_input: Entity<LineInput>,
    pub name_input: Entity<LineInput>,
    pub proxy_input: Entity<LineInput>,
}

impl MultiAccountState {
    pub(crate) fn new(cx: &mut Context<ShellView>) -> Self {
        let status = if cfg!(test) {
            zalo_multi::AccountStatus::default()
        } else {
            zalo_multi::account::status()
        };
        let email_input = cx.new(|cx| {
            let mut input = LineInput::new(cx, "Email tài khoản lza.lol");
            if !status.email.is_empty() {
                input.set_text(status.email.clone(), cx);
            }
            input
        });
        let password_input = cx.new(|cx| LineInput::new_masked(cx, "Mật khẩu"));
        let name_input = cx.new(|cx| LineInput::new(cx, "Tên hồ sơ (vd: Công việc, Bán hàng)"));
        let proxy_input =
            cx.new(|cx| LineInput::new(cx, "Proxy tùy chọn (http:// hoặc socks5://...)"));

        cx.subscribe(&password_input, |this, _, event, cx| {
            if matches!(event, LineEvent::Submit(_)) {
                this.multi_submit_login(cx);
            }
        })
        .detach();
        cx.subscribe(&name_input, |this, _, event, cx| {
            if matches!(event, LineEvent::Submit(_)) {
                match this.multi.edit_mode.clone() {
                    MultiEditMode::Add => this.multi_create_clone(true, cx),
                    MultiEditMode::Rename(old) => this.multi_commit_rename(old, cx),
                    _ => {}
                }
            }
        })
        .detach();
        cx.subscribe(&proxy_input, |this, _, event, cx| {
            if matches!(event, LineEvent::Submit(_)) {
                if let MultiEditMode::Proxy(name) = this.multi.edit_mode.clone() {
                    this.multi_commit_proxy(name, cx);
                }
            }
        })
        .detach();

        Self {
            status,
            clones: Vec::new(),
            busy: false,
            error: None,
            info: None,
            remember: true,
            edit_mode: MultiEditMode::None,
            email_input,
            password_input,
            name_input,
            proxy_input,
        }
    }

    pub(crate) fn running_count(&self) -> usize {
        self.clones.iter().filter(|c| c.running).count()
    }
}

fn snapshot_multi_state(refresh_if_needed: bool) -> (zalo_multi::AccountStatus, Vec<CloneRow>, Option<String>) {
    let mut status = zalo_multi::account::status();
    let mut err: Option<String> = None;
    let now = zalo_multi::account::now_sec();
    let near_expiry = !status.logged_in || status.session_exp.saturating_sub(now) < 180;
    if refresh_if_needed && status.has_refresh && near_expiry {
        match zalo_multi::account::refresh() {
            Ok(next) => status = next,
            Err(e) => {
                if !status.logged_in {
                    err = Some(e.to_string());
                }
            }
        }
    }
    let clones = if status.logged_in {
        zalo_multi::list_profile_names()
            .into_iter()
            .filter_map(|name| {
                let dir = zalo_multi::profile_dir(&name).ok()?;
                Some(CloneRow {
                    running: zalo_multi::is_running(&name, &dir),
                    zalo_name: zalo_multi::read_clone_zalo_name(&dir),
                    proxy: zalo_multi::read_proxy(&dir),
                    name,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    (status, clones, err)
}

impl ShellView {
    /// Refresh ZaloMulti login status and live clone running states off the UI thread.
    pub(crate) fn multi_reload(&mut self, refresh_session: bool, cx: &mut Context<Self>) {
        if cfg!(test) {
            return;
        }
        self.multi.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (status, clones, err) = cx
                .background_executor()
                .spawn(async move { snapshot_multi_state(refresh_session) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                if view.multi.email_input.read(cx).text().trim().is_empty()
                    && !status.email.is_empty()
                {
                    let email = status.email.clone();
                    view.multi
                        .email_input
                        .update(cx, |inp, cx| inp.set_text(email, cx));
                }
                view.multi.status = status;
                view.multi.clones = clones;
                if let Some(msg) = err {
                    view.multi.error = Some(msg);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn multi_submit_login(&mut self, cx: &mut Context<Self>) {
        if self.multi.busy {
            return;
        }
        let email = self.multi.email_input.read(cx).text().trim().to_owned();
        let password = self.multi.password_input.read(cx).text().to_owned();
        let remember = self.multi.remember;
        if email.is_empty() || password.is_empty() {
            self.multi.error = Some("Vui lòng nhập đầy đủ email và mật khẩu.".into());
            cx.notify();
            return;
        }
        self.multi.busy = true;
        self.multi.error = None;
        self.multi.info = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let status = zalo_multi::account::login(&email, &password, remember)
                        .map_err(|e| e.to_string())?;
                    let (_, clones, _) = snapshot_multi_state(false);
                    Ok::<_, String>((status, clones))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                match res {
                    Ok((status, clones)) => {
                        view.multi.status = status;
                        view.multi.clones = clones;
                        view.multi.error = None;
                        view.multi.info = Some("Đăng nhập ZaloMulti thành công.".into());
                        view.multi.password_input.update(cx, |inp, cx| inp.clear(cx));
                    }
                    Err(err) => {
                        view.multi.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn multi_logout(&mut self, cx: &mut Context<Self>) {
        if self.multi.busy {
            return;
        }
        self.multi.busy = true;
        self.multi.error = None;
        self.multi.info = None;
        self.multi.edit_mode = MultiEditMode::None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let status = cx
                .background_executor()
                .spawn(async move { zalo_multi::account::logout() })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                view.multi.status = status;
                view.multi.clones.clear();
                view.multi.info = Some("Đã đăng xuất ZaloMulti.".into());
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn multi_create_clone(&mut self, launch_now: bool, cx: &mut Context<Self>) {
        if self.multi.busy {
            return;
        }
        let name = self.multi.name_input.read(cx).text().trim().to_owned();
        let proxy = self.multi.proxy_input.read(cx).text().trim().to_owned();
        self.multi.busy = true;
        self.multi.error = None;
        self.multi.info = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let clean = zalo_multi::add_profile(&name, "")?;
                    if !proxy.is_empty() {
                        zalo_multi::save_proxy(&clean, &proxy)?;
                    }
                    if launch_now {
                        let _ = zalo_multi::launch_clone(&clean)?;
                    }
                    let (status, clones, _) = snapshot_multi_state(false);
                    Ok::<_, String>((clean, status, clones))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                match res {
                    Ok((clean, status, clones)) => {
                        view.multi.status = status;
                        view.multi.clones = clones;
                        view.multi.edit_mode = MultiEditMode::None;
                        view.multi.name_input.update(cx, |inp, cx| inp.clear(cx));
                        view.multi.proxy_input.update(cx, |inp, cx| inp.clear(cx));
                        view.multi.info = Some(if launch_now {
                            format!("Đã tạo và mở clone \"{clean}\".")
                        } else {
                            format!("Đã tạo clone \"{clean}\".")
                        });
                    }
                    Err(err) => {
                        view.multi.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn multi_launch_clone(&mut self, name: String, cx: &mut Context<Self>) {
        if self.multi.busy {
            return;
        }
        self.multi.busy = true;
        self.multi.error = None;
        self.multi.info = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let out = zalo_multi::launch_clone(&name)?;
                    let (status, clones, _) = snapshot_multi_state(false);
                    Ok::<_, String>((out, status, clones))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                match res {
                    Ok((out, status, clones)) => {
                        view.multi.status = status;
                        view.multi.clones = clones;
                        view.multi.info = Some(if out.already_running {
                            format!("Đã đưa cửa sổ \"{}\" lên trước.", out.name)
                        } else {
                            format!("Đã mở clone \"{}\".", out.name)
                        });
                    }
                    Err(err) => view.multi.error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn multi_stop_clone(&mut self, name: String, cx: &mut Context<Self>) {
        if self.multi.busy {
            return;
        }
        self.multi.busy = true;
        self.multi.error = None;
        self.multi.info = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let _ = zalo_multi::stop_clone(&name)?;
                    let (status, clones, _) = snapshot_multi_state(false);
                    Ok::<_, String>((name, status, clones))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                match res {
                    Ok((name, status, clones)) => {
                        view.multi.status = status;
                        view.multi.clones = clones;
                        view.multi.info = Some(format!("Đã dừng clone \"{name}\"."));
                    }
                    Err(err) => view.multi.error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn multi_stop_all(&mut self, cx: &mut Context<Self>) {
        if self.multi.busy {
            return;
        }
        self.multi.busy = true;
        self.multi.error = None;
        self.multi.info = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (stopped, status, clones) = cx
                .background_executor()
                .spawn(async move {
                    let stopped = zalo_multi::stop_all();
                    let (status, clones, _) = snapshot_multi_state(false);
                    (stopped, status, clones)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                view.multi.status = status;
                view.multi.clones = clones;
                view.multi.info = Some(format!("Đã dừng {stopped} clone."));
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn multi_commit_rename(&mut self, old_name: String, cx: &mut Context<Self>) {
        if self.multi.busy {
            return;
        }
        let new_name = self.multi.name_input.read(cx).text().trim().to_owned();
        self.multi.busy = true;
        self.multi.error = None;
        self.multi.info = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let clean = zalo_multi::rename_profile(&old_name, &new_name)?;
                    let (status, clones, _) = snapshot_multi_state(false);
                    Ok::<_, String>((clean, status, clones))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                match res {
                    Ok((clean, status, clones)) => {
                        view.multi.status = status;
                        view.multi.clones = clones;
                        view.multi.edit_mode = MultiEditMode::None;
                        view.multi.info = Some(format!("Đã đổi tên thành \"{clean}\"."));
                    }
                    Err(err) => view.multi.error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn multi_commit_proxy(&mut self, name: String, cx: &mut Context<Self>) {
        if self.multi.busy {
            return;
        }
        let url = self.multi.proxy_input.read(cx).text().trim().to_owned();
        self.multi.busy = true;
        self.multi.error = None;
        self.multi.info = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    zalo_multi::save_proxy(&name, &url)?;
                    let (status, clones, _) = snapshot_multi_state(false);
                    Ok::<_, String>((name, url, status, clones))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                match res {
                    Ok((name, url, status, clones)) => {
                        view.multi.status = status;
                        view.multi.clones = clones;
                        view.multi.edit_mode = MultiEditMode::None;
                        view.multi.info = Some(if url.is_empty() {
                            format!("Đã tắt proxy cho \"{name}\".")
                        } else {
                            format!("Đã lưu proxy cho \"{name}\" (áp dụng khi mở lại).")
                        });
                    }
                    Err(err) => view.multi.error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn multi_commit_delete(&mut self, name: String, cx: &mut Context<Self>) {
        if self.multi.busy {
            return;
        }
        self.multi.busy = true;
        self.multi.error = None;
        self.multi.info = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    zalo_multi::delete_profile_dir(&name)?;
                    let (status, clones, _) = snapshot_multi_state(false);
                    Ok::<_, String>((name, status, clones))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.multi.busy = false;
                match res {
                    Ok((name, status, clones)) => {
                        view.multi.status = status;
                        view.multi.clones = clones;
                        view.multi.edit_mode = MultiEditMode::None;
                        view.multi.info = Some(format!("Đã xóa clone \"{name}\"."));
                    }
                    Err(err) => view.multi.error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Render the live `RailPopup::MultiAccount` popover.
    pub(crate) fn multi_account_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = crate::color::is_dark();
        let bg = token_rgb("--layer-background", "#ffffff");
        let surface = if dark { 0x2a_2e_35 } else { 0xf4_f6_f9 };
        let input_bg = if dark { 0x1e_21_26 } else { 0xff_ff_ff };
        let border = if dark { 0x3b_40_49 } else { 0xdc_e2_eb };
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#7589a3");
        let blue = 0x00_68_ff;
        let red = 0xd9_30_25;
        let green = 0x22_c5_5e;
        let status = &self.multi.status;

        let mut card = div()
            .id("multi-account-panel")
            .w(px(364.))
            .max_h(px(560.))
            .rounded(px(8.))
            .bg(rgb(bg))
            .border_1()
            .border_color(rgb(border))
            .shadow_lg()
            .flex()
            .flex_col()
            .overflow_hidden();

        // Top header bar
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px(px(14.))
            .py(px(11.))
            .border_b_1()
            .border_color(rgb(border))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .min_w(px(0.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_family(FAMILY_BOLD)
                                    .text_color(rgb(ink))
                                    .child("ZaloMulti • Đa tài khoản"),
                            )
                            .when(status.logged_in && status.donated, |el| {
                                el.child(
                                    div()
                                        .px(px(6.))
                                        .py(px(2.))
                                        .rounded(px(4.))
                                        .bg(rgb(0xf5_9e_0b))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(3.))
                                        .child(fa_icon(FA_CROWN, "★", 9., 0xff_ff_ff))
                                        .child(
                                            div()
                                                .text_size(px(10.))
                                                .font_family(FAMILY_BOLD)
                                                .text_color(rgb(0xff_ff_ff))
                                                .child("PRO"),
                                        ),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(muted))
                            .child(if status.logged_in {
                                format!(
                                    "{} • {}/{} clone",
                                    status.email,
                                    self.multi.clones.len(),
                                    status.max_profiles
                                )
                            } else if !status.hwid_display.is_empty() {
                                format!("Mã máy: {}", status.hwid_display)
                            } else {
                                "Đăng nhập tài khoản lza.lol để quản lý clone".to_owned()
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .id("multi-refresh-btn")
                            .size(px(24.))
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(surface)))
                            .tooltip(|_, cx| tip("Làm mới trạng thái", cx))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.multi_reload(true, cx);
                            }))
                            .child(fa_icon(FA_ROTATE, "↻", 12., muted)),
                    )
                    .child(
                        div()
                            .id("multi-close-btn")
                            .size(px(24.))
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(surface)))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.rail_popup = None;
                                cx.notify();
                            }))
                            .child(fa_icon(FA_XMARK, "×", 13., muted)),
                    ),
            );
        card = card.child(header);

        // Feedback banners (error / info)
        let amber = 0xf5_9e_0b;
        let amber_hover = 0xd9_77_06;
        if let Some(err) = self.multi.error.as_ref() {
            let is_limit_err = err.contains("tối đa") && err.contains("clone");
            card = card.child(
                div()
                    .mx(px(12.))
                    .mt(px(8.))
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .bg(hsla(0.01, 0.85, 0.50, 0.14))
                    .border_1()
                    .border_color(hsla(0.01, 0.85, 0.50, 0.35))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(rgb(if dark { 0xfc_a5_a5 } else { red }))
                            .child(err.clone()),
                    )
                    .when(is_limit_err, |el| {
                        el.child(
                            div()
                                .id("multi-err-buy-btn")
                                .flex_none()
                                .h(px(24.))
                                .px(px(8.))
                                .rounded(px(5.))
                                .bg(rgb(amber))
                                .hover(|s| s.bg(rgb(amber_hover)))
                                .cursor_pointer()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(4.))
                                .on_click(|_, _, _| {
                                    let _ = std::process::Command::new("xdg-open")
                                        .arg(zalo_multi::BUY_MORE_URL)
                                        .spawn();
                                })
                                .child(fa_icon(FA_CART_SHOPPING, "+", 10., 0xff_ff_ff))
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(0xff_ff_ff))
                                        .child("Mua thêm"),
                                ),
                        )
                    }),
            );
        } else if let Some(info) = self.multi.info.as_ref() {
            card = card.child(
                div()
                    .mx(px(12.))
                    .mt(px(8.))
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .bg(hsla(0.58, 0.90, 0.50, 0.12))
                    .border_1()
                    .border_color(hsla(0.58, 0.90, 0.50, 0.28))
                    .text_size(px(12.))
                    .text_color(rgb(if dark { 0x93_c5_fd } else { blue }))
                    .child(info.clone()),
            );
        }

        if !status.logged_in {
            // Login form for https://api.lza.lol/zalomulti
            let remember = self.multi.remember;
            let busy = self.multi.busy;
            card = card.child(
                div()
                    .p(px(14.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(muted))
                            .child("Đăng nhập bằng tài khoản ZaloMulti (lza.lol) để tạo và chạy nhiều tài khoản Zalo song song."),
                    )
                    .child(
                        div()
                            .h(px(34.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .bg(rgb(input_bg))
                            .border_1()
                            .border_color(rgb(border))
                            .flex()
                            .items_center()
                            .child(self.multi.email_input.clone()),
                    )
                    .child(
                        div()
                            .h(px(34.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .bg(rgb(input_bg))
                            .border_1()
                            .border_color(rgb(border))
                            .flex()
                            .items_center()
                            .child(self.multi.password_input.clone()),
                    )
                    .child(
                        div()
                            .id("multi-remember-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.multi.remember = !this.multi.remember;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .size(px(15.))
                                    .rounded(px(3.))
                                    .border_1()
                                    .border_color(rgb(if remember { blue } else { border }))
                                    .when(remember, |el| el.bg(rgb(blue)))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(remember, |el| {
                                        el.child(fa_icon(FA_CHECK, "✓", 9.5, 0xff_ff_ff))
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(ink))
                                    .child("Ghi nhớ đăng nhập (15 ngày)"),
                            ),
                    )
                    .child(
                        div()
                            .id("multi-login-submit")
                            .h(px(36.))
                            .rounded(px(6.))
                            .bg(rgb(blue))
                            .hover(|s| s.bg(rgb(0x00_5a_e0)))
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(13.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(0xff_ff_ff))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.multi_submit_login(cx);
                            }))
                            .child(if busy {
                                "Đang đăng nhập…"
                            } else {
                                "Đăng nhập ZaloMulti"
                            }),
                    )
                    .child(
                        div()
                            .pt(px(4.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .text_size(px(11.5))
                            .text_color(rgb(muted))
                            .child("Xác thực chữ ký Ed25519")
                            .child(
                                div()
                                    .id("multi-open-web")
                                    .cursor_pointer()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(4.))
                                    .text_color(rgb(blue))
                                    .hover(|s| s.underline())
                                    .on_click(|_, _, _| {
                                        let _ = std::process::Command::new("xdg-open")
                                            .arg(zalo_multi::BUY_MORE_URL)
                                            .spawn();
                                    })
                                    .child("Đăng ký / Nâng cấp")
                                    .child(fa_icon(FA_EXTERNAL_LINK, "↗", 10., blue)),
                            ),
                    ),
            );
            return card;
        }

        // Logged-in toolbar: + Thêm clone (or Mua thêm when at limit) / Dừng tất cả
        let running_count = self.multi.running_count();
        let can_add = self.multi.clones.len() < status.max_profiles;
        let max_profiles = status.max_profiles;
        card = card.child(
            div()
                .px(px(12.))
                .pt(px(10.))
                .pb(px(8.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(if can_add {
                    div()
                        .id("multi-add-toggle")
                        .flex_1()
                        .h(px(32.))
                        .px(px(10.))
                        .rounded(px(6.))
                        .bg(rgb(blue))
                        .hover(|s| s.bg(rgb(0x00_5a_e0)))
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .gap(px(6.))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            if this.multi.edit_mode == MultiEditMode::Add {
                                this.multi.edit_mode = MultiEditMode::None;
                            } else {
                                this.multi.error = None;
                                this.multi.info = None;
                                this.multi.name_input.update(cx, |i, cx| i.clear(cx));
                                this.multi.proxy_input.update(cx, |i, cx| i.clear(cx));
                                this.multi.edit_mode = MultiEditMode::Add;
                                let fh = this.multi.name_input.read(cx).focus_handle(cx);
                                window.focus(&fh);
                            }
                            cx.notify();
                        }))
                        .child(fa_icon(FA_PLUS, "+", 11.5, 0xff_ff_ff))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(0xff_ff_ff))
                                .child("Thêm clone"),
                        )
                        .into_any_element()
                } else {
                    div()
                        .id("multi-buy-more-btn")
                        .flex_1()
                        .h(px(32.))
                        .px(px(10.))
                        .rounded(px(6.))
                        .bg(rgb(amber))
                        .hover(|s| s.bg(rgb(amber_hover)))
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .gap(px(6.))
                        .on_click(|_, _, _| {
                            let _ = std::process::Command::new("xdg-open")
                                .arg(zalo_multi::BUY_MORE_URL)
                                .spawn();
                        })
                        .child(fa_icon(FA_CART_SHOPPING, "+", 12., 0xff_ff_ff))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(0xff_ff_ff))
                                .child("Mua thêm"),
                        )
                        .into_any_element()
                })
                .when(running_count > 0, |el| {
                    let stop_fg = if dark { 0xfc_a5_a5 } else { red };
                    el.child(
                        div()
                            .id("multi-stop-all")
                            .h(px(32.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .bg(hsla(0.01, 0.85, 0.50, 0.14))
                            .hover(|s| s.bg(hsla(0.01, 0.85, 0.50, 0.24)))
                            .cursor_pointer()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .gap(px(5.))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.multi_stop_all(cx);
                            }))
                            .child(fa_icon(FA_STOP, "■", 10.5, stop_fg))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(stop_fg))
                                    .child(format!("Dừng tất cả ({running_count})")),
                            ),
                    )
                }),
        );

        // Quota notice banner when at limit
        if !can_add {
            card = card.child(
                div()
                    .mx(px(12.))
                    .mb(px(8.))
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .bg(hsla(0.10, 0.92, 0.50, 0.13))
                    .border_1()
                    .border_color(hsla(0.10, 0.92, 0.50, 0.34))
                    .text_size(px(11.5))
                    .text_color(rgb(if dark { 0xfc_d3_4d } else { 0xb4_53_09 }))
                    .child(zalo_multi::profiles::limit_error(max_profiles)),
            );
        }

        // Inline forms (Add / Rename / Proxy / ConfirmDelete)
        match self.multi.edit_mode.clone() {
            MultiEditMode::None => {}
            MultiEditMode::Add => {
                card = card.child(
                    div()
                        .mx(px(12.))
                        .mb(px(8.))
                        .p(px(10.))
                        .rounded(px(6.))
                        .bg(rgb(surface))
                        .border_1()
                        .border_color(rgb(border))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(ink))
                                .child("Tạo hồ sơ clone mới"),
                        )
                        .child(
                            div()
                                .h(px(30.))
                                .px(px(8.))
                                .rounded(px(5.))
                                .bg(rgb(input_bg))
                                .border_1()
                                .border_color(rgb(border))
                                .flex()
                                .items_center()
                                .child(self.multi.name_input.clone()),
                        )
                        .child(
                            div()
                                .h(px(30.))
                                .px(px(8.))
                                .rounded(px(5.))
                                .bg(rgb(input_bg))
                                .border_1()
                                .border_color(rgb(border))
                                .flex()
                                .items_center()
                                .child(self.multi.proxy_input.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_end()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .id("multi-add-cancel")
                                        .h(px(26.))
                                        .px(px(9.))
                                        .rounded(px(5.))
                                        .bg(rgb(input_bg))
                                        .border_1()
                                        .border_color(rgb(border))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .text_color(rgb(ink))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.multi.edit_mode = MultiEditMode::None;
                                            cx.notify();
                                        }))
                                        .child("Hủy"),
                                )
                                .child(
                                    div()
                                        .id("multi-add-only")
                                        .h(px(26.))
                                        .px(px(9.))
                                        .rounded(px(5.))
                                        .bg(rgb(input_bg))
                                        .border_1()
                                        .border_color(rgb(border))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .text_color(rgb(ink))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.multi_create_clone(false, cx);
                                        }))
                                        .child("Chỉ tạo"),
                                )
                                .child(
                                    div()
                                        .id("multi-add-open")
                                        .h(px(26.))
                                        .px(px(10.))
                                        .rounded(px(5.))
                                        .bg(rgb(blue))
                                        .hover(|s| s.bg(rgb(0x00_5a_e0)))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(0xff_ff_ff))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.multi_create_clone(true, cx);
                                        }))
                                        .child("Tạo & Mở ngay"),
                                ),
                        ),
                );
            }
            MultiEditMode::Rename(old) => {
                let old_submit = old.clone();
                card = card.child(
                    div()
                        .mx(px(12.))
                        .mb(px(8.))
                        .p(px(10.))
                        .rounded(px(6.))
                        .bg(rgb(surface))
                        .border_1()
                        .border_color(rgb(border))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(ink))
                                .child(format!("Đổi tên clone \"{old}\"")),
                        )
                        .child(
                            div()
                                .h(px(30.))
                                .px(px(8.))
                                .rounded(px(5.))
                                .bg(rgb(input_bg))
                                .border_1()
                                .border_color(rgb(border))
                                .flex()
                                .items_center()
                                .child(self.multi.name_input.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_end()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .id("multi-ren-cancel")
                                        .h(px(26.))
                                        .px(px(9.))
                                        .rounded(px(5.))
                                        .bg(rgb(input_bg))
                                        .border_1()
                                        .border_color(rgb(border))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .text_color(rgb(ink))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.multi.edit_mode = MultiEditMode::None;
                                            cx.notify();
                                        }))
                                        .child("Hủy"),
                                )
                                .child(
                                    div()
                                        .id("multi-ren-save")
                                        .h(px(26.))
                                        .px(px(10.))
                                        .rounded(px(5.))
                                        .bg(rgb(blue))
                                        .hover(|s| s.bg(rgb(0x00_5a_e0)))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(0xff_ff_ff))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.multi_commit_rename(old_submit.clone(), cx);
                                        }))
                                        .child("Lưu tên"),
                                ),
                        ),
                );
            }
            MultiEditMode::Proxy(name) => {
                let name_submit = name.clone();
                card = card.child(
                    div()
                        .mx(px(12.))
                        .mb(px(8.))
                        .p(px(10.))
                        .rounded(px(6.))
                        .bg(rgb(surface))
                        .border_1()
                        .border_color(rgb(border))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(ink))
                                .child(format!("Proxy cho \"{name}\" (để trống để tắt)")),
                        )
                        .child(
                            div()
                                .h(px(30.))
                                .px(px(8.))
                                .rounded(px(5.))
                                .bg(rgb(input_bg))
                                .border_1()
                                .border_color(rgb(border))
                                .flex()
                                .items_center()
                                .child(self.multi.proxy_input.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_end()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .id("multi-proxy-cancel")
                                        .h(px(26.))
                                        .px(px(9.))
                                        .rounded(px(5.))
                                        .bg(rgb(input_bg))
                                        .border_1()
                                        .border_color(rgb(border))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .text_color(rgb(ink))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.multi.edit_mode = MultiEditMode::None;
                                            cx.notify();
                                        }))
                                        .child("Hủy"),
                                )
                                .child(
                                    div()
                                        .id("multi-proxy-save")
                                        .h(px(26.))
                                        .px(px(10.))
                                        .rounded(px(5.))
                                        .bg(rgb(blue))
                                        .hover(|s| s.bg(rgb(0x00_5a_e0)))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(0xff_ff_ff))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.multi_commit_proxy(name_submit.clone(), cx);
                                        }))
                                        .child("Lưu proxy"),
                                ),
                        ),
                );
            }
            MultiEditMode::ConfirmDelete(name) => {
                let name_del = name.clone();
                card = card.child(
                    div()
                        .mx(px(12.))
                        .mb(px(8.))
                        .p(px(10.))
                        .rounded(px(6.))
                        .bg(hsla(0.01, 0.85, 0.50, 0.12))
                        .border_1()
                        .border_color(hsla(0.01, 0.85, 0.50, 0.35))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(ink))
                                .child(format!(
                                    "Xóa vĩnh viễn toàn bộ dữ liệu đăng nhập của clone \"{name}\"?"
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_end()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .id("multi-del-cancel")
                                        .h(px(26.))
                                        .px(px(9.))
                                        .rounded(px(5.))
                                        .bg(rgb(input_bg))
                                        .border_1()
                                        .border_color(rgb(border))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .text_color(rgb(ink))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.multi.edit_mode = MultiEditMode::None;
                                            cx.notify();
                                        }))
                                        .child("Hủy"),
                                )
                                .child(
                                    div()
                                        .id("multi-del-confirm")
                                        .h(px(26.))
                                        .px(px(10.))
                                        .rounded(px(5.))
                                        .bg(rgb(red))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(0xff_ff_ff))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.multi_commit_delete(name_del.clone(), cx);
                                        }))
                                        .child("Xác nhận xóa"),
                                ),
                        ),
                );
            }
        }

        // Scrollable clone list
        let current_label = crate::active_profile_name()
            .map(|p| format!("Cửa sổ này (Clone: {p})"))
            .unwrap_or_else(|| "Tài khoản chính".to_owned());
        let me_sub = self
            .display_name
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Đang hoạt động".to_owned());

        let mut list = div()
            .id("multi-clone-list")
            .flex_1()
            .min_h(px(120.))
            .max_h(px(310.))
            .overflow_y_scroll()
            .px(px(12.))
            .pb(px(10.))
            .flex()
            .flex_col()
            .gap(px(6.))
            // Pinned current window row
            .child(
                div()
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .bg(rgb(surface))
                    .border_1()
                    .border_color(rgb(border))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .min_w(px(0.))
                            .child(div().size(px(8.)).rounded(px(4.)).bg(rgb(green)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w(px(0.))
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(ink))
                                            .child(current_label),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(rgb(muted))
                                            .child(me_sub),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(10.))
                            .bg(hsla(0.39, 0.72, 0.45, 0.16))
                            .text_size(px(11.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(green))
                            .child("Đang mở"),
                    ),
            );

        if self.multi.clones.is_empty() {
            list = list.child(
                div()
                    .py(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.5))
                    .text_color(rgb(muted))
                    .child("Chưa có hồ sơ clone nào — bấm «+ Thêm clone» để tạo."),
            );
        } else {
            for (idx, row) in self.multi.clones.iter().enumerate() {
                let name_focus = row.name.clone();
                let name_toggle = row.name.clone();
                let name_ren = row.name.clone();
                let name_prx = row.name.clone();
                let proxy_cur = row.proxy.clone();
                let name_del = row.name.clone();
                let running = row.running;
                let has_proxy = !row.proxy.is_empty();
                let sub = match (&row.zalo_name, has_proxy) {
                    (Some(zn), true) => format!("Zalo: {zn} • Proxy"),
                    (Some(zn), false) => format!("Zalo: {zn}"),
                    (None, true) => "Chưa đăng nhập Zalo • Proxy".to_owned(),
                    (None, false) => "Chưa đăng nhập Zalo".to_owned(),
                };
                let stop_fg = if dark { 0xfc_a5_a5 } else { red };

                list = list.child(
                    div()
                        .id(("multi-clone-row", idx as u64))
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .bg(rgb(input_bg))
                        .border_1()
                        .border_color(rgb(border))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(px(6.))
                        .child(
                            div()
                                .id(("multi-clone-info", idx as u64))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .min_w(px(0.))
                                .flex_1()
                                .when(running, |el| {
                                    el.cursor_pointer()
                                        .tooltip(|_, cx| tip("Nhấn để đưa cửa sổ lên trước", cx))
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, _, cx| {
                                                this.multi_launch_clone(name_focus.clone(), cx);
                                            },
                                        ))
                                })
                                .child(
                                    div()
                                        .flex_none()
                                        .size(px(8.))
                                        .rounded(px(4.))
                                        .bg(rgb(if running { green } else { 0x94_a3_b8 })),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w(px(0.))
                                        .child(
                                            div()
                                                .text_size(px(12.5))
                                                .font_family(FAMILY_SEMIBOLD)
                                                .text_color(rgb(ink))
                                                .truncate()
                                                .child(row.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(rgb(muted))
                                                .truncate()
                                                .child(sub),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(4.))
                                // Single fixed Open/Stop toggle button
                                .child(
                                    div()
                                        .id(("multi-toggle-btn", idx as u64))
                                        .w(px(62.))
                                        .h(px(26.))
                                        .rounded(px(5.))
                                        .when(!running, |el| {
                                            el.bg(rgb(blue)).hover(|s| s.bg(rgb(0x00_5a_e0)))
                                        })
                                        .when(running, |el| {
                                            el.bg(hsla(0.01, 0.85, 0.50, 0.16))
                                                .hover(|s| s.bg(hsla(0.01, 0.85, 0.50, 0.28)))
                                        })
                                        .cursor_pointer()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .justify_center()
                                        .gap(px(5.))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            if running {
                                                this.multi_stop_clone(name_toggle.clone(), cx);
                                            } else {
                                                this.multi_launch_clone(name_toggle.clone(), cx);
                                            }
                                        }))
                                        .child(if running {
                                            fa_icon(FA_STOP, "■", 9.5, stop_fg)
                                        } else {
                                            fa_icon(FA_PLAY, "▶", 9.5, 0xff_ff_ff)
                                        })
                                        .child(
                                            div()
                                                .text_size(px(11.5))
                                                .font_family(FAMILY_SEMIBOLD)
                                                .text_color(rgb(if running {
                                                    stop_fg
                                                } else {
                                                    0xff_ff_ff
                                                }))
                                                .child(if running { "Dừng" } else { "Mở" }),
                                        ),
                                )
                                // Gear icon (rename / clone settings)
                                .child(
                                    div()
                                        .id(("multi-ren-btn", idx as u64))
                                        .size(px(26.))
                                        .rounded(px(5.))
                                        .hover(|s| s.bg(rgb(surface)))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .tooltip(|_, cx| tip("Cài đặt / Đổi tên clone", cx))
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, window, cx| {
                                                let cur = name_ren.clone();
                                                this.multi.error = None;
                                                this.multi.name_input.update(cx, |i, cx| {
                                                    i.set_text(cur.clone(), cx);
                                                });
                                                this.multi.edit_mode = MultiEditMode::Rename(cur);
                                                let fh =
                                                    this.multi.name_input.read(cx).focus_handle(cx);
                                                window.focus(&fh);
                                                cx.notify();
                                            },
                                        ))
                                        .child(fa_icon(FA_GEAR, "⚙", 12.5, muted)),
                                )
                                // Globe icon (proxy)
                                .child(
                                    div()
                                        .id(("multi-prx-btn", idx as u64))
                                        .size(px(26.))
                                        .rounded(px(5.))
                                        .hover(|s| s.bg(rgb(surface)))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .tooltip(|_, cx| tip("Cấu hình Proxy", cx))
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, window, cx| {
                                                let cur_p = proxy_cur.clone();
                                                this.multi.error = None;
                                                this.multi.proxy_input.update(cx, |i, cx| {
                                                    i.set_text(cur_p, cx);
                                                });
                                                this.multi.edit_mode =
                                                    MultiEditMode::Proxy(name_prx.clone());
                                                let fh =
                                                    this.multi.proxy_input.read(cx).focus_handle(cx);
                                                window.focus(&fh);
                                                cx.notify();
                                            },
                                        ))
                                        .child(fa_icon(
                                            FA_GLOBE,
                                            "🌐",
                                            12.5,
                                            if has_proxy { blue } else { muted },
                                        )),
                                )
                                // Trash icon (delete)
                                .child(
                                    div()
                                        .id(("multi-del-btn", idx as u64))
                                        .size(px(26.))
                                        .rounded(px(5.))
                                        .when(!running, |el| {
                                            el.hover(|s| s.bg(hsla(0.01, 0.85, 0.50, 0.14)))
                                                .cursor_pointer()
                                                .on_click(cx.listener(
                                                    move |this, _: &ClickEvent, _, cx| {
                                                        this.multi.error = None;
                                                        this.multi.edit_mode =
                                                            MultiEditMode::ConfirmDelete(
                                                                name_del.clone(),
                                                            );
                                                        cx.notify();
                                                    },
                                                ))
                                        })
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .tooltip(move |_, cx| {
                                            tip(
                                                if running {
                                                    "Dừng clone trước khi xóa"
                                                } else {
                                                    "Xóa clone"
                                                },
                                                cx,
                                            )
                                        })
                                        .child(fa_icon(
                                            FA_TRASH,
                                            "✕",
                                            12.,
                                            if running { border } else { muted },
                                        )),
                                ),
                        ),
                );
            }
        }
        card = card.child(list);

        // Footer: open clone folder + logout
        let logout_fg = if dark { 0xfc_a5_a5 } else { red };
        card = card.child(
            div()
                .px(px(12.))
                .py(px(9.))
                .border_t_1()
                .border_color(rgb(border))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .id("multi-open-folder")
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(5.))
                        .hover(|s| s.bg(rgb(surface)))
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .on_click(|_, _, _| {
                            let dir = zalo_multi::account_clone_root()
                                .unwrap_or_else(zalo_multi::profile_root);
                            let _ = std::fs::create_dir_all(&dir);
                            let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                        })
                        .child(fa_icon(FA_FOLDER_OPEN, "📁", 12., muted))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(rgb(muted))
                                .child("Mở thư mục clone"),
                        ),
                )
                .child(
                    div()
                        .id("multi-logout-btn")
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(5.))
                        .hover(|s| s.bg(hsla(0.01, 0.85, 0.50, 0.12)))
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(5.))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.multi_logout(cx);
                        }))
                        .child(fa_icon(FA_RIGHT_FROM_BRACKET, "↪", 11.5, logout_fg))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(rgb(logout_fg))
                                .child("Đăng xuất"),
                        ),
                ),
        );

        card
    }
}
