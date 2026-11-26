//! Official 1:1 Zalo PC "Quản lý nhóm" UI (`compact1360.js` + `compact1707.js`).
//!
//! Layout matches Zalo PC desktop:
//! 1. Non-admin lock banner (when regular member).
//! 2. Section "Cho phép các thành viên trong nhóm:" with 5 square checkboxes
//!    (checked = members ARE allowed).
//! 3. Neutral 6px section divider.
//! 4. Group switches: Membership Approval (with inline pending link),
//!    Highlight Admin Messages, Allow New Members to Read History,
//!    Hide Member List, and Inline Group Invite Link (toggle + link box
//!    with Copy / Share / Renew + "Tham gia nhóm khác bằng link").
//! 5. Admin navigation rows: "Chặn khỏi nhóm", "Duyệt thành viên",
//!    "Trưởng & phó nhóm" (owner), "Nâng cấp thành cộng đồng" (owner).
//! 6. Footer danger button "Giải tán nhóm" (owner, 2-step confirmation).

use gpui::{
    div, prelude::*, px, rgb, ClickEvent, ClipboardItem, Context, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};

use crate::account::{
    group_role_label, is_community, setting_copy, GroupManageDlg, GroupManageTab, GroupSettingKey,
    MEMBER_ALLOW_CHECKBOXES,
};
use crate::color::{border_subtle, link_fg, on_accent_fg, token_rgb};
use crate::dialogs::{Dialog, Person};
use crate::fonts::{self, FAMILY_SEMIBOLD};
use crate::info::GroupRoleAction;
use crate::shell::ShellView;

fn section_gap() -> impl IntoElement {
    let bg = if crate::color::is_dark() {
        0x14_14_14
    } else {
        token_rgb("--layer-background-subtle", "#eef0f1")
    };
    div().flex_none().h(px(6.)).w_full().bg(rgb(bg))
}

impl ShellView {
    /// Main 1:1 Zalo PC "Quản lý nhóm" view (`compact1360.js:707-993`).
    pub(crate) fn group_manage_main(
        &self,
        d: &GroupManageDlg,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let danger = token_rgb("--R60", "#c31818");
        let accent = token_rgb("--B55", "#0068ff");
        let link_c = link_fg();
        let line = border_subtle();

        let mine = self.self_member_ids();
        let role = group_role_label(&mine, &d.creator, &d.admins);
        let owner = !d.loading && role == "Trưởng nhóm";
        let can_manage = !d.loading && role != "Thành viên";
        let community = is_community(d.group_type, d.total_member);
        let conv_id = d.conv_id;
        let row_ink = if can_manage { ink } else { muted };

        let nav_row = |id: &'static str,
                       icon: char,
                       label: String,
                       badge: Option<usize>,
                       is_danger: bool| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .flex_row()
                .items_center()
                .gap(px(12.))
                .h(px(46.))
                .px(px(16.))
                .cursor_pointer()
                .hover(move |st| st.bg(rgb(hover)))
                .child(
                    div()
                        .text_color(rgb(if is_danger { danger } else { muted }))
                        .child(fonts::icon_el(icon, "•", 20.)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(14.))
                        .text_color(rgb(if is_danger { danger } else { ink }))
                        .truncate()
                        .child(label),
                )
                .when_some(badge.filter(|&n| n > 0), |el, n| {
                    el.child(
                        div()
                            .px(px(7.))
                            .py(px(1.))
                            .rounded(px(10.))
                            .bg(rgb(subtle))
                            .text_size(px(12.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child(n.to_string()),
                    )
                })
                .child(
                    div()
                        .text_color(rgb(muted))
                        .child(fonts::icon_el('\u{EA95}', "›", 16.)),
                )
        };

        let switch_row = |id: &'static str,
                          label: String,
                          on: bool,
                          key: GroupSettingKey,
                          enabled: bool,
                          cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(16.))
                .py(px(11.))
                .when(enabled, |el| {
                    el.cursor_pointer()
                        .hover(move |st| st.bg(rgb(hover)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_group_setting(conv_id, key, !on, cx);
                        }))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .text_color(rgb(if enabled { ink } else { muted }))
                        .child(crate::media::wrap_ui_note(&label, 236.)),
                )
                .child(crate::info::switch_pill(on))
        };

        let link_url = d.link.url.clone();
        let link_display = link_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_owned();
        let link_copy = link_url.clone();
        let link_preview = link_url.clone();
        let link_enabled = d.link.enabled;
        let link_busy = d.link_loading;

        div()
            .flex()
            .flex_col()
            .w_full()
            // ── Non-admin banner (compact1360.js:712) ──
            .when(!can_manage && !d.loading, |el| {
                el.child(
                    div()
                        .mx(px(16.))
                        .mt(px(12.))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .bg(rgb(subtle))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_color(rgb(muted))
                                .child(fonts::icon_el('\u{EC20}', "🔒", 15.)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(12.5))
                                .text_color(rgb(muted))
                                .child(setting_copy(
                                    "STR_BANNER_DISABLE_SETTING",
                                    "Tính năng chỉ dành cho quản trị viên",
                                )),
                        ),
                )
            })
            .when(d.loading, |el| {
                el.child(
                    div()
                        .px(px(16.))
                        .pt(px(12.))
                        .text_size(px(12.5))
                        .text_color(rgb(muted))
                        .child("Đang tải cài đặt nhóm…"),
                )
            })
            .when_some(d.error.clone(), |el, err| {
                el.child(
                    div()
                        .px(px(16.))
                        .pt(px(8.))
                        .text_size(px(12.5))
                        .text_color(rgb(danger))
                        .child(err),
                )
            })
            // ── Section 1: "Cho phép các thành viên trong nhóm:" (5 square checkboxes, 1:1 compact1360.js:721-825) ──
            .child(
                div()
                    .flex()
                    .flex_col()
                    .pt(px(14.))
                    .pb(px(8.))
                    .child(
                        div()
                            .px(px(16.))
                            .pb(px(6.))
                            .text_size(px(14.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(row_ink))
                            .child(setting_copy(
                                "STR_TITLE_ALLOW_GROUP_SETTING",
                                "Cho phép các thành viên trong nhóm:",
                            )),
                    )
                    .children(MEMBER_ALLOW_CHECKBOXES.iter().enumerate().map(
                        |(i, (key, str_key, fallback))| {
                            let locked = d.settings.get(*key);
                            let allowed = !locked;
                            let key = *key;
                            let label = setting_copy(str_key, fallback);
                            let interactive = can_manage && !d.settings_busy;
                            div()
                                .id(("gm-allow", i as u64))
                                .flex()
                                .flex_none()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .gap(px(12.))
                                .px(px(16.))
                                .py(px(9.))
                                .when(interactive, |el| {
                                    el.cursor_pointer()
                                        .hover(move |st| st.bg(rgb(hover)))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            // Checkbox is "allowed" = !locked; clicking flips locked flag.
                                            this.toggle_group_setting(conv_id, key, !locked, cx);
                                        }))
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .text_size(px(14.))
                                        .line_height(px(20.))
                                        .text_color(rgb(row_ink))
                                        .child(crate::media::wrap_ui_note(&label, 250.)),
                                )
                                .child(crate::info::check_box(allowed))
                        },
                    )),
            )
            .child(section_gap())
            // ── Section 2: Switches + Inline Group Link (1:1 compact1360.js:826-958 & compact1707.js) ──
            .child(
                div()
                    .flex()
                    .flex_col()
                    .py(px(6.))
                    .child(switch_row(
                        "gm-sw-join-appr",
                        setting_copy(
                            "STR_ADMIN_MEMBERSHIP_APPROVAL",
                            "Chế độ phê duyệt thành viên mới",
                        ),
                        d.settings.join_appr,
                        GroupSettingKey::JoinAppr,
                        can_manage && !d.settings_busy,
                        cx,
                    ))
                    .when(can_manage && !d.pending.is_empty(), |el| {
                        let cnt = d.pending.len();
                        el.child(
                            div()
                                .id("gm-pending-inline")
                                .px(px(16.))
                                .pb(px(8.))
                                .cursor_pointer()
                                .text_size(px(13.))
                                .text_color(rgb(link_c))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut() {
                                        dlg.tab = GroupManageTab::Pending;
                                    }
                                    this.load_group_pending(conv_id, cx);
                                    cx.notify();
                                }))
                                .child(format!("Xem danh sách chờ duyệt ({cnt})")),
                        )
                    })
                    .child(switch_row(
                        "gm-sw-sign-admin",
                        setting_copy(
                            "STR_MARK_ADMIN_MESSAGE",
                            "Đánh dấu tin nhắn từ trưởng/phó nhóm",
                        ),
                        d.settings.sign_admin_msg,
                        GroupSettingKey::SignAdminMsg,
                        can_manage && !d.settings_busy,
                        cx,
                    ))
                    .child(switch_row(
                        "gm-sw-history",
                        setting_copy(
                            "STR_ALLOW_GET_HISTORY",
                            "Cho phép thành viên mới đọc tin nhắn gần nhất",
                        ),
                        d.settings.enable_msg_history,
                        GroupSettingKey::EnableMsgHistory,
                        can_manage && !d.settings_busy,
                        cx,
                    ))
                    .child(switch_row(
                        "gm-sw-hide-mem",
                        setting_copy(
                            "STR_GROUP_PRIVACY_HIDE_MEMBER_PRESENCE",
                            "Chỉ trưởng/phó nhóm xem được đầy đủ danh sách thành viên",
                        ),
                        d.settings.lock_view_member,
                        GroupSettingKey::LockViewMember,
                        can_manage && !d.settings_busy,
                        cx,
                    ))
                    // ── Inline "Cho phép dùng link tham gia nhóm" (compact1707.js:245-300) ──
                    .child(
                        div()
                            .id("gm-sw-link")
                            .flex()
                            .flex_none()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(px(12.))
                            .px(px(16.))
                            .py(px(11.))
                            .when(can_manage && !link_busy, |el| {
                                el.cursor_pointer()
                                    .hover(move |st| st.bg(rgb(hover)))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.mutate_group_link(conv_id, !link_enabled, cx);
                                    }))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(14.))
                                    .line_height(px(20.))
                                    .text_color(rgb(row_ink))
                                    .child("Cho phép dùng link tham gia nhóm"),
                            )
                            .child(crate::info::switch_pill(link_enabled)),
                    )
                    .when(link_enabled && !link_url.is_empty(), |el| {
                        el.child(
                            div()
                                .mx(px(16.))
                                .mb(px(8.))
                                .px(px(10.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(subtle))
                                .border_1()
                                .border_color(line)
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .id("gm-link-open-preview")
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .cursor_pointer()
                                        .text_size(px(13.))
                                        .text_color(rgb(link_c))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.open_group_link_preview(link_preview.clone(), cx);
                                            cx.notify();
                                        }))
                                        .child(link_display),
                                )
                                .child(
                                    div()
                                        .id("gm-link-copy")
                                        .px(px(7.))
                                        .py(px(4.))
                                        .rounded(px(4.))
                                        .hover(move |st| st.bg(rgb(hover)))
                                        .cursor_pointer()
                                        .text_size(px(12.))
                                        .text_color(rgb(ink))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                link_copy.clone(),
                                            ));
                                            this.set_note("Đã sao chép liên kết nhóm");
                                            cx.notify();
                                        }))
                                        .child("Chép"),
                                )
                                .when(owner && !link_busy, |row| {
                                    row.child(
                                        div()
                                            .id("gm-link-renew")
                                            .px(px(7.))
                                            .py(px(4.))
                                            .rounded(px(4.))
                                            .hover(move |st| st.bg(rgb(hover)))
                                            .cursor_pointer()
                                            .text_size(px(12.))
                                            .text_color(rgb(ink))
                                            .on_click(cx.listener(
                                                move |this, _: &ClickEvent, _, cx| {
                                                    this.mutate_group_link(conv_id, true, cx);
                                                },
                                            ))
                                            .child("Đổi link"),
                                    )
                                }),
                        )
                    })
                    .child(
                        div()
                            .id("gm-link-join-other")
                            .px(px(16.))
                            .py(px(6.))
                            .cursor_pointer()
                            .text_size(px(13.))
                            .text_color(rgb(link_c))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.open_group_link_input(cx);
                                cx.notify();
                            }))
                            .child("Tham gia nhóm khác bằng link…"),
                    ),
            )
            // ── Section 3: Admin entries (1:1 compact1360.js:959-976) ──
            .when(can_manage, |el| {
                let blocked_n = d.blocked_members.len();
                let pending_n = d.pending.len();
                let community_label = if d.community_armed {
                    "Xác nhận nâng cấp thành cộng đồng".to_owned()
                } else {
                    "Nâng cấp thành cộng đồng".to_owned()
                };
                el.child(section_gap()).child(
                    div()
                        .flex()
                        .flex_col()
                        .py(px(4.))
                        .child(
                            nav_row(
                                "gm-nav-blocked",
                                '\u{EA30}',
                                setting_copy("STR_BLOCK_GROUP_MEMBER", "Chặn khỏi nhóm"),
                                Some(blocked_n),
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut() {
                                        dlg.tab = GroupManageTab::Blocked;
                                        dlg.adding_block = false;
                                        dlg.tab_note = None;
                                    }
                                    this.load_group_blocked_members(conv_id, cx);
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            nav_row(
                                "gm-nav-pending",
                                '\u{EB45}',
                                "Duyệt thành viên".to_owned(),
                                Some(pending_n),
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut() {
                                        dlg.tab = GroupManageTab::Pending;
                                        dlg.tab_note = None;
                                    }
                                    this.load_group_pending(conv_id, cx);
                                    cx.notify();
                                },
                            )),
                        )
                        .when(owner, |col| {
                            col.child(
                                nav_row(
                                    "gm-nav-admins",
                                    '\u{EC35}',
                                    setting_copy("STR_OWNER_ADMIN", "Trưởng & phó nhóm"),
                                    None,
                                    false,
                                )
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, _, cx| {
                                        if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut()
                                        {
                                            dlg.tab = GroupManageTab::Admins;
                                            dlg.adding_admin = false;
                                            dlg.owner_transfer_armed = None;
                                            dlg.tab_note = None;
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                            .when(!community, |c2| {
                                c2.child(
                                    nav_row(
                                        "gm-nav-community",
                                        '\u{EABA}',
                                        community_label,
                                        None,
                                        false,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            let armed = matches!(
                                                this.dialog.as_ref(),
                                                Some(Dialog::GroupManage(dlg)) if dlg.community_armed
                                            );
                                            if !armed {
                                                if let Some(Dialog::GroupManage(dlg)) =
                                                    this.dialog.as_mut()
                                                {
                                                    dlg.community_armed = true;
                                                    dlg.disperse_armed = false;
                                                }
                                            } else {
                                                this.dialog = None;
                                                this.upgrade_community_now(conv_id, cx);
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                                .when(d.community_armed, |c3| {
                                    c3.child(
                                        div()
                                            .px(px(16.))
                                            .pb(px(8.))
                                            .text_size(px(12.))
                                            .text_color(rgb(muted))
                                            .child("Sau khi nâng cấp thành cộng đồng sẽ không thể hoàn tác. Bấm lần nữa để xác nhận."),
                                    )
                                })
                                .when_some(d.community_quota.clone(), |c3, q| {
                                    c3.child(
                                        div()
                                            .px(px(16.))
                                            .pb(px(6.))
                                            .text_size(px(12.))
                                            .text_color(rgb(muted))
                                            .child(q),
                                    )
                                })
                            })
                        })
                        .child(
                            nav_row(
                                "gm-remove-topic",
                                '\u{EDCF}',
                                "Xóa chủ đề nhóm hiện tại".to_owned(),
                                None,
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    this.remove_group_topic_now(conv_id, cx);
                                },
                            )),
                        )
                        .when_some(d.analyse_summary.clone(), |col, summary| {
                            col.child(
                                div()
                                    .px(px(16.))
                                    .py(px(6.))
                                    .text_size(px(12.))
                                    .text_color(rgb(muted))
                                    .child(summary),
                            )
                        }),
                )
            })
            // ── Section 4: Footer "Giải tán nhóm" (owner only, 1:1 compact1360.js:977-992) ──
            .when(owner, |el| {
                let btn_label = if d.disperse_armed {
                    "Xác nhận giải tán nhóm"
                } else {
                    "Giải tán nhóm"
                };
                el.child(section_gap()).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .p(px(16.))
                        .child(
                            div()
                                .id("gm-disperse-btn")
                                .h(px(38.))
                                .rounded(px(6.))
                                .bg(rgb(if d.disperse_armed { danger } else { subtle }))
                                .hover(move |st| {
                                    st.bg(rgb(if d.disperse_armed { danger } else { hover }))
                                })
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_size(px(14.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(if d.disperse_armed {
                                    on_accent_fg()
                                } else {
                                    danger
                                }))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let armed = matches!(
                                        this.dialog.as_ref(),
                                        Some(Dialog::GroupManage(dlg)) if dlg.disperse_armed
                                    );
                                    if !armed {
                                        if let Some(Dialog::GroupManage(dlg)) =
                                            this.dialog.as_mut()
                                        {
                                            dlg.disperse_armed = true;
                                            dlg.community_armed = false;
                                        }
                                    } else {
                                        this.dialog = None;
                                        this.disperse_group_now(conv_id, cx);
                                    }
                                    cx.notify();
                                }))
                                .child(btn_label),
                        )
                        .when(d.disperse_armed, |col| {
                            col.child(
                                div()
                                    .text_size(px(12.5))
                                    .line_height(px(18.))
                                    .text_color(rgb(danger))
                                    .child(setting_copy(
                                        "STR_CONFIRM_DELETE_GROUP",
                                        "Mời tất cả mọi người rời nhóm và xóa tin nhắn? Nhóm đã giải tán sẽ KHÔNG THỂ khôi phục.",
                                    )),
                            )
                        }),
                )
            })
            .when(false, |el| el.bg(rgb(accent)))
    }

    /// Renders sub-pages inside "Quản lý nhóm": Pending, Blocked, Admins (1:1 Zalo PC).
    pub(crate) fn group_manage_subpage(
        &self,
        d: &GroupManageDlg,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let danger = token_rgb("--R60", "#c31818");
        let accent = token_rgb("--B55", "#0068ff");
        let accent_fg = on_accent_fg();
        let line = border_subtle();
        let conv_id = d.conv_id;
        let mine = self.self_member_ids();

        match d.tab {
            GroupManageTab::Pending => div()
                .flex()
                .flex_col()
                .p(px(16.))
                .gap(px(10.))
                .when_some(d.tab_note.clone(), |el, note| {
                    el.child(div().text_size(px(12.5)).text_color(rgb(danger)).child(note))
                })
                .when(d.pending_loading, |el| {
                    el.child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child("Đang tải yêu cầu tham gia…"),
                    )
                })
                .when(!d.pending_loading && d.pending.is_empty(), |el| {
                    el.child(
                        div()
                            .py(px(24.))
                            .text_center()
                            .text_size(px(13.5))
                            .text_color(rgb(muted))
                            .child("Không có yêu cầu chờ duyệt."),
                    )
                })
                .children(d.pending.iter().enumerate().map(|(i, m)| {
                    let uid_ok = m.uid.clone();
                    let uid_no = m.uid.clone();
                    let person = Person {
                        uid: m.uid.clone(),
                        name: m.name.clone(),
                        avatar: String::new(),
                        note: String::new(),
                    };
                    div()
                        .id(("gm-pend-row", i as u64))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .py(px(6.))
                        .border_b_1()
                        .border_color(line)
                        .child(self.dialog_avatar(&person, 36.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(14.))
                                .text_color(rgb(ink))
                                .child(m.name.clone()),
                        )
                        .child(
                            div()
                                .id(("gm-pend-ok", i as u64))
                                .px(px(10.))
                                .py(px(5.))
                                .rounded(px(4.))
                                .bg(rgb(accent))
                                .text_size(px(12.))
                                .font_family(FAMILY_SEMIBOLD)
                                .text_color(rgb(accent_fg))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.review_group_pending(conv_id, uid_ok.clone(), true, cx);
                                }))
                                .child("Duyệt"),
                        )
                        .child(
                            div()
                                .id(("gm-pend-no", i as u64))
                                .px(px(10.))
                                .py(px(5.))
                                .rounded(px(4.))
                                .bg(rgb(subtle))
                                .hover(move |st| st.bg(rgb(hover)))
                                .text_size(px(12.))
                                .text_color(rgb(danger))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.review_group_pending(conv_id, uid_no.clone(), false, cx);
                                }))
                                .child("Từ chối"),
                        )
                }))
                .into_any_element(),

            GroupManageTab::Blocked => {
                let blockable: Vec<(String, String)> = d
                    .members
                    .iter()
                    .filter(|(uid, _)| {
                        crate::info::can_kick_member(&mine, &d.creator, &d.admins, uid)
                    })
                    .cloned()
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .p(px(16.))
                    .gap(px(10.))
                    .child(
                        div()
                            .id("gm-add-block-toggle")
                            .h(px(36.))
                            .rounded(px(6.))
                            .bg(rgb(subtle))
                            .hover(move |st| st.bg(rgb(hover)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_size(px(13.5))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut() {
                                    dlg.adding_block = !dlg.adding_block;
                                }
                                cx.notify();
                            }))
                            .child(if d.adding_block {
                                "Đóng chọn thành viên"
                            } else {
                                "+ Chặn thành viên khỏi nhóm"
                            }),
                    )
                    .when(d.adding_block, |el| {
                        el.child(
                            div()
                                .p(px(10.))
                                .rounded(px(6.))
                                .bg(rgb(subtle))
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .when(blockable.is_empty(), |box_el| {
                                    box_el.child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(rgb(muted))
                                            .child("Không có thành viên nào có thể chặn."),
                                    )
                                })
                                .children(blockable.into_iter().enumerate().map(
                                    |(i, (uid, name))| {
                                        let uid_b = uid.clone();
                                        let name_b = name.clone();
                                        div()
                                            .id(("gm-pick-blk", i as u64))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(8.))
                                            .py(px(4.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .truncate()
                                                    .text_size(px(13.))
                                                    .text_color(rgb(ink))
                                                    .child(name),
                                            )
                                            .child(
                                                div()
                                                    .id(("gm-pick-blk-go", i as u64))
                                                    .px(px(8.))
                                                    .py(px(3.))
                                                    .rounded(px(4.))
                                                    .bg(rgb(danger))
                                                    .text_size(px(12.))
                                                    .text_color(rgb(0xffffff))
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.block_group_member(
                                                                conv_id,
                                                                uid_b.clone(),
                                                                name_b.clone(),
                                                                cx,
                                                            );
                                                        },
                                                    ))
                                                    .child("Chặn"),
                                            )
                                    },
                                )),
                        )
                    })
                    .when_some(d.tab_note.clone(), |el, note| {
                        el.child(div().text_size(px(12.5)).text_color(rgb(danger)).child(note))
                    })
                    .when(d.blocked_loading, |el| {
                        el.child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(muted))
                                .child("Đang tải danh sách bị chặn…"),
                        )
                    })
                    .when(!d.blocked_loading && d.blocked_members.is_empty(), |el| {
                        el.child(
                            div()
                                .py(px(24.))
                                .text_center()
                                .text_size(px(13.5))
                                .text_color(rgb(muted))
                                .child("Chưa có thành viên nào bị chặn vào nhóm."),
                        )
                    })
                    .children(d.blocked_members.iter().enumerate().map(|(i, m)| {
                        let uid_unblock = m.uid.clone();
                        let name_unblock = m.name.clone();
                        let person = Person {
                            uid: m.uid.clone(),
                            name: m.name.clone(),
                            avatar: String::new(),
                            note: String::new(),
                        };
                        div()
                            .id(("gm-blk-row", i as u64))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.))
                            .py(px(6.))
                            .border_b_1()
                            .border_color(line)
                            .child(self.dialog_avatar(&person, 36.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(px(14.))
                                    .text_color(rgb(ink))
                                    .child(m.name.clone()),
                            )
                            .child(
                                div()
                                    .id(("gm-unblock", i as u64))
                                    .px(px(10.))
                                    .py(px(5.))
                                    .rounded(px(4.))
                                    .bg(rgb(subtle))
                                    .hover(move |st| st.bg(rgb(hover)))
                                    .text_size(px(12.))
                                    .text_color(rgb(ink))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.unblock_group_member(
                                            conv_id,
                                            uid_unblock.clone(),
                                            name_unblock.clone(),
                                            cx,
                                        );
                                    }))
                                    .child("Bỏ chặn"),
                            )
                    }))
                    .into_any_element()
            }

            GroupManageTab::Admins => {
                let name_of = |uid: &str| -> String {
                    if mine.contains(uid) {
                        return "Bạn".to_owned();
                    }
                    d.members
                        .iter()
                        .find(|(id, _)| id == uid)
                        .map(|(_, n)| n.clone())
                        .filter(|n| !n.is_empty())
                        .or_else(|| {
                            self.profiles
                                .map
                                .get(uid)
                                .map(|p| p.name.clone())
                                .filter(|n| !n.is_empty())
                        })
                        .unwrap_or_else(|| uid.to_owned())
                };
                let creator_name = name_of(&d.creator);
                let creator_person = Person {
                    uid: d.creator.clone(),
                    name: creator_name.clone(),
                    avatar: String::new(),
                    note: String::new(),
                };
                let promotable: Vec<(String, String)> = d
                    .members
                    .iter()
                    .filter(|(uid, _)| {
                        !mine.contains(uid)
                            && uid != &d.creator
                            && !d.admins.contains(uid)
                    })
                    .cloned()
                    .collect();
                let transfer_candidates: Vec<(String, String)> = d
                    .members
                    .iter()
                    .filter(|(uid, _)| !mine.contains(uid) && uid != &d.creator)
                    .cloned()
                    .collect();

                div()
                    .flex()
                    .flex_col()
                    .p(px(16.))
                    .gap(px(12.))
                    // ── Trưởng nhóm ──
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(muted))
                                    .child("Trưởng nhóm"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(10.))
                                    .py(px(6.))
                                    .child(self.dialog_avatar(&creator_person, 38.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(px(14.5))
                                            .font_family(FAMILY_SEMIBOLD)
                                            .text_color(rgb(ink))
                                            .child(creator_name),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(0xe8_a3_17))
                                            .child(fonts::icon_el('\u{EC35}', "★", 16.)),
                                    ),
                            ),
                    )
                    // ── Phó nhóm ──
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .pt(px(6.))
                            .border_t_1()
                            .border_color(line)
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(muted))
                                    .child(format!("Phó nhóm ({})", d.admins.len())),
                            )
                            .when(d.admins.is_empty(), |el| {
                                el.child(
                                    div()
                                        .py(px(6.))
                                        .text_size(px(13.))
                                        .text_color(rgb(muted))
                                        .child("Chưa có phó nhóm nào."),
                                )
                            })
                            .children(d.admins.iter().enumerate().map(|(i, uid)| {
                                let nm = name_of(uid);
                                let uid_rm = uid.clone();
                                let nm_rm = nm.clone();
                                let person = Person {
                                    uid: uid.clone(),
                                    name: nm.clone(),
                                    avatar: String::new(),
                                    note: String::new(),
                                };
                                div()
                                    .id(("gm-adm-row", i as u64))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(10.))
                                    .py(px(6.))
                                    .child(self.dialog_avatar(&person, 36.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(px(14.))
                                            .text_color(rgb(ink))
                                            .child(nm),
                                    )
                                    .child(
                                        div()
                                            .id(("gm-adm-rm", i as u64))
                                            .px(px(8.))
                                            .py(px(4.))
                                            .rounded(px(4.))
                                            .bg(rgb(subtle))
                                            .hover(move |st| st.bg(rgb(hover)))
                                            .cursor_pointer()
                                            .text_size(px(12.))
                                            .text_color(rgb(danger))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.set_group_member_role(
                                                    conv_id,
                                                    uid_rm.clone(),
                                                    nm_rm.clone(),
                                                    GroupRoleAction::DemoteAdmin,
                                                    cx,
                                                );
                                            }))
                                            .child("Gỡ quyền"),
                                    )
                            }))
                            .child(
                                div()
                                    .id("gm-add-admin-btn")
                                    .mt(px(4.))
                                    .h(px(36.))
                                    .rounded(px(6.))
                                    .bg(rgb(subtle))
                                    .hover(move |st| st.bg(rgb(hover)))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .text_size(px(13.5))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(ink))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        if let Some(Dialog::GroupManage(dlg)) = this.dialog.as_mut()
                                        {
                                            dlg.adding_admin = !dlg.adding_admin;
                                        }
                                        cx.notify();
                                    }))
                                    .child(if d.adding_admin {
                                        "Đóng danh sách chọn"
                                    } else {
                                        "+ Thêm phó nhóm"
                                    }),
                            )
                            .when(d.adding_admin, |el| {
                                el.child(
                                    div()
                                        .p(px(10.))
                                        .rounded(px(6.))
                                        .bg(rgb(subtle))
                                        .flex()
                                        .flex_col()
                                        .gap(px(6.))
                                        .when(promotable.is_empty(), |b| {
                                            b.child(
                                                div()
                                                    .text_size(px(12.5))
                                                    .text_color(rgb(muted))
                                                    .child("Tất cả thành viên đã là quản trị viên."),
                                            )
                                        })
                                        .children(promotable.into_iter().enumerate().map(
                                            |(i, (uid, name))| {
                                                let u = uid.clone();
                                                let n = name.clone();
                                                div()
                                                    .id(("gm-pick-adm", i as u64))
                                                    .flex()
                                                    .flex_row()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap(px(8.))
                                                    .py(px(4.))
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w(px(0.))
                                                            .truncate()
                                                            .text_size(px(13.))
                                                            .text_color(rgb(ink))
                                                            .child(name),
                                                    )
                                                    .child(
                                                        div()
                                                            .id(("gm-pick-adm-go", i as u64))
                                                            .px(px(8.))
                                                            .py(px(3.))
                                                            .rounded(px(4.))
                                                            .bg(rgb(accent))
                                                            .text_size(px(12.))
                                                            .font_family(FAMILY_SEMIBOLD)
                                                            .text_color(rgb(accent_fg))
                                                            .cursor_pointer()
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.set_group_member_role(
                                                                        conv_id,
                                                                        u.clone(),
                                                                        n.clone(),
                                                                        GroupRoleAction::PromoteAdmin,
                                                                        cx,
                                                                    );
                                                                },
                                                            ))
                                                            .child("Thêm"),
                                                    )
                                            },
                                        )),
                                )
                            }),
                    )
                    // ── Chuyển quyền trưởng nhóm ──
                    .when(!transfer_candidates.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .pt(px(8.))
                                .border_t_1()
                                .border_color(line)
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .font_family(FAMILY_SEMIBOLD)
                                        .text_color(rgb(muted))
                                        .child("Chuyển quyền trưởng nhóm"),
                                )
                                .children(transfer_candidates.into_iter().enumerate().map(
                                    |(i, (uid, name))| {
                                        let armed = d.owner_transfer_armed.as_deref()
                                            == Some(uid.as_str());
                                        let u = uid.clone();
                                        let n = name.clone();
                                        div()
                                            .id(("gm-xfer-row", i as u64))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(8.))
                                            .py(px(4.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .truncate()
                                                    .text_size(px(13.5))
                                                    .text_color(rgb(ink))
                                                    .child(name),
                                            )
                                            .child(
                                                div()
                                                    .id(("gm-xfer-btn", i as u64))
                                                    .px(px(8.))
                                                    .py(px(4.))
                                                    .rounded(px(4.))
                                                    .bg(rgb(if armed { danger } else { subtle }))
                                                    .cursor_pointer()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(if armed {
                                                        0xffffff
                                                    } else {
                                                        ink
                                                    }))
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            if armed {
                                                                this.set_group_member_role(
                                                                    conv_id,
                                                                    u.clone(),
                                                                    n.clone(),
                                                                    GroupRoleAction::TransferOwner,
                                                                    cx,
                                                                );
                                                                if let Some(Dialog::GroupManage(
                                                                    dlg,
                                                                )) = this.dialog.as_mut()
                                                                {
                                                                    dlg.owner_transfer_armed = None;
                                                                    dlg.tab =
                                                                        GroupManageTab::Permissions;
                                                                }
                                                            } else if let Some(
                                                                Dialog::GroupManage(dlg),
                                                            ) = this.dialog.as_mut()
                                                            {
                                                                dlg.owner_transfer_armed =
                                                                    Some(u.clone());
                                                                this.set_note(
                                                                    "Bấm lần nữa để xác nhận chuyển trưởng nhóm",
                                                                );
                                                            }
                                                            cx.notify();
                                                        },
                                                    ))
                                                    .child(if armed {
                                                        "Xác nhận?"
                                                    } else {
                                                        "Chuyển"
                                                    }),
                                            )
                                    },
                                )),
                        )
                    })
                    .into_any_element()
            }

            GroupManageTab::Permissions => div().into_any_element(),
        }
    }
}
