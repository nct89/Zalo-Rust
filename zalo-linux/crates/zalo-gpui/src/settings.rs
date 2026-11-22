//! Settings modal: `.setting-menu` 228 px + 10 Zalo pages, plus Linux "Advanced".
//! Page models follow the live Zalo PC 26.10.10 dialog (CDP, 11/11/2026).
//! Local rows persist through [`crate::prefs`]; server-backed rows stay grey
//! with the reason in a tooltip.

use std::collections::{HashMap, HashSet};

use gpui::{
    div, prelude::*, px, rgb, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};

use crate::color::{str_vi, token_rgb};
use crate::fonts::{self, ic, FAMILY_SEMIBOLD};
use crate::shell::ShellView;
use crate::widgets::tip;
use crate::window_frame::CsdCorners;

/// `.setting-menu` width (live DOM 229 incl. 1 px border).
pub const SETTING_MENU_W: f32 = 228.0;
/// `.zl-modal__dialog` width on Zalo PC 26.10.10 (live DOM: 800).
pub const SETTING_MODAL_W: f32 = 800.0;

/// Left-menu entries, same order as the decoded settings modal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    /// General settings.
    #[default]
    General,
    /// Account & Security.
    Security,
    /// Privacy.
    Privacy,
    /// Sync & Backup.
    Sync,
    /// Data Management.
    Data,
    /// Appearance.
    Theme,
    /// Notifications.
    Notify,
    /// Messages.
    Messages,
    /// Call Settings.
    Call,
    /// Utilities.
    Utilities,
    /// Linux plugin `advanced-settings`: ringtone, ringback.
    Advanced,
}

impl SettingsPage {
    /// Zalo's ten pages, then the Linux "Advanced" plugin page.
    pub const ALL: [SettingsPage; 11] = [
        SettingsPage::General,
        SettingsPage::Security,
        SettingsPage::Privacy,
        SettingsPage::Sync,
        SettingsPage::Data,
        SettingsPage::Theme,
        SettingsPage::Notify,
        SettingsPage::Messages,
        SettingsPage::Call,
        SettingsPage::Utilities,
        SettingsPage::Advanced,
    ];

    /// Catalogue title.
    pub fn title(self) -> &'static str {
        match self {
            Self::General => str_vi("STR_GENERAL", "Cài đặt chung"),
            Self::Security => str_vi("STR_SETTINGS_ACCOUNT_AND_SECURITY", "Tài khoản và bảo mật"),
            Self::Privacy => str_vi("STR_SETTINGS_PRIVACY", "Quyền riêng tư"),
            Self::Sync => "Đồng bộ và sao lưu",
            Self::Data => str_vi("STR_RESOURCE_MANAGE", "Quản lý dữ liệu"),
            Self::Theme => str_vi("STR_SETTINGS_THEME", "Giao diện"),
            Self::Notify => str_vi("STR_CONFIG_NOTI", "Thông báo"),
            Self::Messages => str_vi("STR_SETTING_MSG", "Tin nhắn"),
            Self::Call => str_vi("STR_VIDEO_CALL_SETTINGS", "Cài đặt cuộc gọi").trim(),
            Self::Utilities => str_vi("STR_UTILITIES", "Tiện ích"),
            Self::Advanced => "Nâng cao",
        }
    }

    fn icon(self) -> (char, &'static str) {
        match self {
            Self::General => (ic::SETTING, "⚙"),
            Self::Security => (ic::SECURITY, "🔒"),
            Self::Privacy => (ic::PRIVACY, "👁"),
            Self::Sync => ('\u{EA27}', "↻"),
            Self::Data => (ic::STORAGE, "▣"),
            Self::Theme => (ic::THEME, "◐"),
            Self::Notify => (ic::NOTIF_ON, "🔔"),
            Self::Messages => (ic::MESSAGES, "✉"),
            Self::Call => (ic::PHONE, "☎"),
            Self::Utilities => (ic::UTILITY, "☰"),
            Self::Advanced => ('\u{EA3A}', "♪"),
        }
    }
}

impl ShellView {
    /// Dimmed backdrop + centered dialog. Click outside or × closes.
    pub(crate) fn settings_overlay(
        &self,
        _corners: CsdCorners,
        height: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("settings-layer")
            .absolute()
            .inset(px(0.))
            .child(
                div()
                    .id("settings-backdrop")
                    .absolute()
                    .inset(px(0.))
                    .occlude()
                    .bg(crate::color::modal_scrim())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_settings();
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
                            .id("settings-card")
                            .occlude()
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation()
                            })
                            .child(self.settings_modal(height, cx)),
                    ),
            )
    }

    fn settings_modal(&self, height: f32, cx: &mut Context<Self>) -> impl IntoElement {
        // `.zl-modal__dialog` 800 × (window − 32), white menu 229 + right pane
        // with a 56px header bar (page title + vertically centered × button).
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        div()
            .id("settings-modal")
            .relative()
            .flex()
            .flex_row()
            .w(px(SETTING_MODAL_W))
            .h(px(height))
            .max_h(px(668.))
            .rounded(px(8.))
            .overflow_hidden()
            .bg(rgb(subtle))
            .shadow_lg()
            .child(self.settings_menu(cx))
            .child(
                div()
                    .id("setting-right")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .justify_start()
                    .overflow_hidden()
                    .bg(rgb(subtle))
                    .child(
                        div()
                            .id("setting-right-header")
                            .flex()
                            .flex_none()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .h(px(56.))
                            .px(px(20.))
                            .bg(rgb(paper))
                            .border_b_1()
                            .border_color(crate::color::border_subtle())
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .line_height(px(24.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(ink))
                                    .child(self.settings_page.title()),
                            )
                            .child(
                                div()
                                    .id("settings-close")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(32.))
                                    .rounded(px(16.))
                                    .cursor_pointer()
                                    .text_color(rgb(ink))
                                    .hover(|style| style.bg(rgb(hover)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_settings();
                                        cx.notify();
                                    }))
                                    .child(fonts::icon_el('\u{EAA1}', "×", 16.)),
                            ),
                    )
                    .child(self.settings_body(cx)),
            )
    }

    fn settings_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // `.setting-menu__header` 56 (title 18 / 500); items 40 tall,
        // margin 4 8, padding 0 8, radius 6, icon 18 + 10, text 14.
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let selected = token_rgb("--layer-background-selected", "#e5f1ff");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let page = self.settings_page;
        div()
            .id("setting-menu")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SETTING_MENU_W + 1.0))
            .h_full()
            .bg(rgb(paper))
            .border_r_1()
            .border_color(crate::color::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(56.))
                    .px(px(16.))
                    .text_size(px(18.))
                    .line_height(px(27.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child(str_vi("STR_ZALO_SETTING", "Cài đặt")),
            )
            .children(SettingsPage::ALL.into_iter().map(move |item| {
                let on = item == page;
                let (code, fallback) = item.icon();
                div()
                    .id(("setting-menu", item as u64))
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .h(px(40.))
                    .mx(px(8.))
                    .mt(px(4.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(rgb(ink))
                    .when(on, |el| el.bg(rgb(selected)))
                    .when(!on, |el| el.hover(|style| style.bg(rgb(hover))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_page = item;
                        this.refresh_settings_cache(item);
                        this.ensure_privacy_loaded(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(20.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(fonts::icon_el(code, fallback, 18.)),
                    )
                    .child(
                        div()
                            .ml(px(10.))
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(14.))
                            .line_height(px(21.))
                            .truncate()
                            .child(item.title()),
                    )
            }))
    }

    fn settings_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.settings_page == SettingsPage::Data {
            return self.data_page(cx).into_any_element();
        }
        if self.settings_page == SettingsPage::Sync {
            return self.sync_page(cx).into_any_element();
        }
        let sections = self.page_sections(self.settings_page);
        let blocked_panel = (self.settings_page == SettingsPage::Privacy
            && self.privacy.blocked_open)
            .then(|| self.blocked_panel(cx).into_any_element());
        let theme_cards = (self.settings_page == SettingsPage::Theme)
            .then(|| self.theme_cards_el(cx).into_any_element());
        div()
            .id("setting-section")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .justify_start()
            .overflow_y_scroll()
            .pt(px(12.))
            .pb(px(24.))
            .px(px(12.))
            .when_some(theme_cards, |el, cards| el.child(cards))
            .children(
                sections
                    .into_iter()
                    .enumerate()
                    .map(|(si, section)| self.section_el(si, section, cx).into_any_element()),
            )
            .when_some(blocked_panel, |el, panel| el.child(panel))
            .into_any_element()
    }

    /// Electron "Appearance Settings": 3 horizontal mini preview cards (Light / Dark / System).
    fn theme_cards_el(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let mode = if self.prefs.bool("followSystem", false) {
            "system"
        } else if self.prefs.bool("dark", false) {
            "dark"
        } else {
            "light"
        };
        let mini_half = |dark: bool, w_rail: f32, show_bubbles: bool| {
            let rail_bg = if dark { 0x12_14_16 } else { 0x00_68_ff };
            let list_bg = if dark { 0x22_26_2b } else { 0xff_ff_ff };
            let chat_bg = if dark { 0x16_19_1d } else { 0xeb_ec_f0 };
            let bar_bg = if dark { 0x38_3e_47 } else { 0xdc_e1_e8 };
            let in_bub = if dark { 0x26_2a_30 } else { 0xff_ff_ff };
            let out_bub = if dark { 0x1a_3a_64 } else { 0xe5_f1_ff };
            div()
                .flex()
                .flex_row()
                .flex_1()
                .h_full()
                .child(div().w(px(w_rail)).h_full().bg(rgb(rail_bg)))
                .child(
                    div()
                        .w(px(34.))
                        .h_full()
                        .bg(rgb(list_bg))
                        .p(px(5.))
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .child(div().h(px(6.)).w(px(22.)).rounded(px(2.)).bg(rgb(bar_bg)))
                        .child(div().h(px(5.)).w(px(18.)).rounded(px(2.)).bg(rgb(bar_bg)))
                        .child(div().h(px(5.)).w(px(24.)).rounded(px(2.)).bg(rgb(bar_bg)))
                        .child(div().h(px(5.)).w(px(16.)).rounded(px(2.)).bg(rgb(bar_bg))),
                )
                .when(show_bubbles, |el| {
                    el.child(
                        div()
                            .flex_1()
                            .h_full()
                            .bg(rgb(chat_bg))
                            .p(px(6.))
                            .flex()
                            .flex_col()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(5.))
                                    .child(
                                        div()
                                            .h(px(12.))
                                            .w(px(42.))
                                            .rounded(px(4.))
                                            .bg(rgb(in_bub)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .justify_end()
                                            .w_full()
                                            .child(
                                                div()
                                                    .h(px(12.))
                                                    .w(px(38.))
                                                    .rounded(px(4.))
                                                    .bg(rgb(out_bub)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .h(px(10.))
                                            .w(px(32.))
                                            .rounded(px(4.))
                                            .bg(rgb(in_bub)),
                                    ),
                            )
                            .child(div().h(px(10.)).w_full().rounded(px(3.)).bg(rgb(list_bg))),
                    )
                })
        };
        let cards: [(&'static str, &'static str); 3] = [
            ("light", "Sáng"),
            ("dark", "Tối"),
            ("system", "Hệ Thống"),
        ];
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(16.))
                    .h(px(32.))
                    .text_size(px(16.))
                    .line_height(px(32.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child(crate::i18n::t("Cài đặt giao diện")),
            )
            .child(
                div()
                    .mt(px(8.))
                    .mb(px(24.))
                    .rounded(px(8.))
                    .bg(rgb(paper))
                    .p(px(16.))
                    .flex()
                    .flex_row()
                    .gap(px(14.))
                    .children(cards.into_iter().enumerate().map(|(ci, (val, label))| {
                        let on = mode == val;
                        let preview = match val {
                            "light" => mini_half(false, 12., true).into_any_element(),
                            "dark" => mini_half(true, 12., true).into_any_element(),
                            _ => div()
                                .flex()
                                .flex_row()
                                .size_full()
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .h_full()
                                        .overflow_hidden()
                                        .child(mini_half(false, 10., true)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .h_full()
                                        .overflow_hidden()
                                        .child(mini_half(true, 0., true)),
                                )
                                .into_any_element(),
                        };
                        div()
                            .id(("theme-card", ci as u64))
                            .flex()
                            .flex_1()
                            .flex_col()
                            .items_center()
                            .gap(px(10.))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_pref(
                                    "theme",
                                    serde_json::Value::String(val.to_owned()),
                                    window,
                                    cx,
                                );
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .w_full()
                                    .h(px(88.))
                                    .rounded(px(8.))
                                    .overflow_hidden()
                                    .border_2()
                                    .border_color(if on {
                                        rgb(blue).into()
                                    } else {
                                        crate::color::border_subtle()
                                    })
                                    .child(preview),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .text_color(rgb(if on { blue } else { muted }))
                                            .child(fonts::icon_el(
                                                if on { '\u{ECEF}' } else { '\u{EDD3}' },
                                                "○",
                                                20.,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .when(on, |d| d.font_family(FAMILY_SEMIBOLD))
                                            .text_color(rgb(if on { ink } else { muted }))
                                            .child(crate::i18n::t(label)),
                                    ),
                            )
                    })),
            )
    }

    /// Xuất/Nhập. The PC-to-phone backup card is not on this page.
    fn sync_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let blue = token_rgb("--B55", "#0068ff");
        let dark = crate::color::is_dark();
        let card_bg = if dark { 0x2a_2c_31 } else { 0xf4_f6_f8 };
        let card_border = if dark { 0x3a_3e_46 } else { 0xe1_e5_ea };
        let hover = if dark { 0x32_35_3b } else { 0xea_ee_f4 };
        let card = |key: &'static str,
                    art: gpui::AnyElement,
                    title: &'static str,
                    desc: String,
                    one_line: bool| {
            div()
                .id(key)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .w_full()
                .mt(px(8.))
                .px(px(12.))
                .py(px(10.))
                .rounded(px(8.))
                .bg(rgb(card_bg))
                .border_1()
                .border_color(rgb(card_border))
                .cursor_pointer()
                .hover(|st| st.bg(rgb(hover)))
                .child(art)
                .child(
                    div()
                        .mt(px(6.))
                        .text_size(px(16.))
                        .line_height(px(22.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(ink))
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .w_full()
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .text_color(rgb(muted))
                        .text_center()
                        .when(one_line, |el| el.whitespace_nowrap())
                        .child(if one_line {
                            desc
                        } else {
                            crate::media::wrap_ui_note(&desc, 520.)
                        }),
                )
        };
        let sync_enabled = self.sync_ui.config.is_enabled_for(zalo_sync::SyncSource::Manual);
        let backup_enabled = self.backup_ui.config.enabled;
        div()
            .id("setting-sync")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(16.))
            .pt(px(12.))
            .pb(px(12.))
            .text_color(rgb(ink))
            .child(
                div()
                    .text_size(px(16.))
                    .line_height(px(22.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child(str_vi("STR_SELECT_METHOD_SETTING", "Chọn phương thức:")),
            )
            .when(sync_enabled, |el| {
                el.child(
                    card(
                        "sync-phone-to-pc",
                        sync_art_phone_to_pc(blue),
                        str_vi("STR_PROFILE_SYNC_MESSAGES", "Đồng bộ tin nhắn"),
                        str_vi(
                            "STR_SYNC_METHOD_SETTING_DESC",
                            "Đồng bộ tin nhắn từ điện thoại sang máy tính này",
                        )
                        .to_owned(),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = false;
                        this.start_manual_sync(cx);
                    })),
                )
            })
            .when(backup_enabled, |el| {
                el.child(
                    card(
                        "backup-pc-to-phone",
                        sync_art_pc_to_phone(blue),
                        str_vi("STR_BACKUP_METHOD_SETTING_TITLE", "Sao lưu tin nhắn"),
                        str_vi(
                            "STR_BACKUP_METHOD_SETTING_DESC",
                            "Sao lưu tin nhắn từ máy tính này sang điện thoại",
                        )
                        .to_owned(),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = false;
                        this.open_backup_popup(cx);
                    })),
                )
            })
            .child(
                card(
                    "sync-pc-export",
                    sync_art_pc_to_pc(blue),
                    "Xuất dữ liệu/ Nhập dữ liệu",
                    "Xuất dữ liệu khi chuyển máy, đổi máy tính mới".to_owned(),
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.open_pc_export(cx);
                })),
            )
    }

    /// "Data Management" like the live dialog: total, "Storage Location" +
    /// "Change", usage bar (Zalo / other / free), three cards, refresh line.
    fn data_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let paper = token_rgb("--layer-background", "#ffffff");
        let blue = token_rgb("--B55", "#0068ff");
        let sky = 0x29_b6_f6;
        let c = self.settings_cache.clone().unwrap_or_default();
        let zalo = c.media + c.cache + c.system;
        let total = c.fs_total.max(1) as f32;
        let zalo_frac = (zalo as f32 / total).clamp(0.005, 1.0);
        let other_frac =
            (c.fs_used.saturating_sub(zalo) as f32 / total).clamp(0.0, 1.0 - zalo_frac);
        let root = crate::media::media_root()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let card = |key: &'static str,
                    icon: char,
                    title: &'static str,
                    desc: &'static str,
                    size: u64,
                    trailing: Option<(char, SAction)>,
                    cx: &mut Context<Self>| {
            div()
                .id(key)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(16.))
                .px(px(16.))
                .py(px(14.))
                .mb(px(12.))
                .rounded(px(8.))
                .bg(rgb(paper))
                .child(fonts::icon_el(icon, "•", 20.))
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
                                .child(crate::i18n::t(&title)),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(muted))
                                .child(crate::media::wrap_ui_note(desc, 400.)),
                        ),
                )
                .child(
                    div()
                        .text_size(px(14.))
                        .font_family(FAMILY_SEMIBOLD)
                        .child(fmt_size(size)),
                )
                .when_some(trailing, |el, (code, act)| {
                    el.child(
                        div()
                            .id((key, 1u64))
                            .size(px(28.))
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|st| {
                                st.bg(rgb(token_rgb("--layer-background-hover", "#f1f2f4")))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_setting_action(act, cx);
                                cx.notify();
                            }))
                            .child(fonts::icon_el(code, "›", 20.)),
                    )
                })
        };
        div()
            .id("setting-data")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .justify_start()
            .overflow_y_scroll()
            .px(px(28.))
            .pt(px(12.))
            .pb(px(24.))
            .text_color(rgb(ink))
            .child(
                div()
                    .text_size(px(16.))
                    .font_family(FAMILY_SEMIBOLD)
                    .child("Dữ liệu Zalo"),
            )
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(30.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(blue))
                    .child(fmt_size(zalo)),
            )
            .child(
                div()
                    .mt(px(14.))
                    .flex()
                    .flex_row()
                    .items_end()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.))
                            .child(div().text_size(px(13.)).child("Vị trí lưu trữ"))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .truncate()
                                    .child(format!("({}) {root}", c.mount)),
                            ),
                    )
                    .child(
                        div()
                            .id("data-move")
                            .px(px(14.))
                            .h(px(28.))
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .bg(rgb(crate::color::token_rgb(
                                "--button-secondary-neutral-normal",
                                "#e5e7eb",
                            )))
                            .hover(|st| {
                                st.bg(rgb(crate::color::token_rgb(
                                    "--button-secondary-neutral-hover",
                                    "#d6d9de",
                                )))
                            })
                            .font_family(FAMILY_SEMIBOLD)
                            .text_size(px(13.))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.run_setting_action(SAction::MoveData, cx);
                                cx.notify();
                            }))
                            .child("Thay đổi"),
                    ),
            )
            .child(
                div()
                    .mt(px(10.))
                    .h(px(14.))
                    .w_full()
                    .rounded(px(3.))
                    .overflow_hidden()
                    .flex()
                    .flex_row()
                    .bg(rgb(0x5a_69_81))
                    .child(div().h_full().w(gpui::relative(zalo_frac)).bg(rgb(blue)))
                    .child(div().h_full().w(gpui::relative(other_frac)).bg(rgb(sky))),
            )
            .child(
                div()
                    .mt(px(8.))
                    .mb(px(16.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(16.))
                    .text_size(px(13.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().size(px(10.)).bg(rgb(blue)))
                            .child("Zalo"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().size(px(10.)).bg(rgb(sky)))
                            .child("Ứng dụng khác"),
                    )
                    .child(div().flex_1())
                    .child(c.free.clone()),
            )
            .child(card(
                "data-media",
                '\u{ECC5}',
                "Tin nhắn media",
                "Dữ liệu ảnh, video, file, và tệp ghi âm của bạn được lưu trên máy này",
                c.media,
                Some(('\u{EA1C}', SAction::OpenMedia)),
                cx,
            ))
            .child(card(
                "data-cache",
                '\u{ED7D}',
                "Bộ nhớ đệm",
                "Dữ liệu tạm sinh ra để tăng tốc ứng dụng, có thể dọn dẹp để giải phóng dung lượng",
                c.cache,
                Some(('\u{EDCF}', SAction::ClearCache)),
                cx,
            ))
            .child(card(
                "data-system",
                '\u{ED7D}',
                "Dữ liệu hệ thống",
                "Tin nhắn văn bản và dữ liệu cần thiết để vận hành ứng dụng",
                c.system,
                None,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(4.))
                    .text_size(px(13.))
                    .child(format!("Cập nhật lần cuối lúc {}.", c.updated))
                    .child(
                        div()
                            .id("data-refresh")
                            .text_color(rgb(blue))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_settings_cache(SettingsPage::Data);
                                cx.notify();
                            }))
                            .child("Làm mới"),
                    ),
            )
    }

    fn section_el(&self, si: usize, section: Section, cx: &mut Context<Self>) -> impl IntoElement {
        // Label 16 px, line 32. The white card pads 12 px above the first
        // control and 12 px below the last, with 12 px between rows, so the
        // content sits evenly. Cards stay as tall as their own rows.
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let paper = token_rgb("--layer-background", "#ffffff");
        let n = section.rows.len();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(16.))
                    .h(px(32.))
                    .text_size(px(16.))
                    .line_height(px(32.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child(crate::i18n::t(&section.title)),
            )
            .when_some(section.desc, |el, d| {
                el.child(
                    div()
                        .px(px(16.))
                        .text_size(px(13.))
                        .line_height(px(18.2))
                        .text_color(rgb(muted))
                        .child(crate::media::wrap_ui_note(&crate::i18n::t(&d), 520.)),
                )
            })
            .when(n > 0, |el| {
                el.child(
                    div()
                        .mt(px(8.))
                        .mb(px(24.))
                        .rounded(px(8.))
                        .bg(rgb(paper))
                        .py(px(12.))
                        .gap(px(12.))
                        .flex()
                        .flex_col()
                        .children(section.rows.into_iter().enumerate().map(|(ri, row)| {
                            self.row_el(si * 100 + ri, row, cx).into_any_element()
                        })),
                )
            })
            .when(n == 0, |el| el.child(div().h(px(24.))))
    }

    fn row_el(&self, id: usize, row: SRow, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let disabled = token_rgb("--text-disabled", "#a6afbd");
        let blue = token_rgb("--B55", "#0068ff");
        let off_reason = match &row.ctl {
            Ctl::Off(reason) => Some(*reason),
            _ => None,
        };
        let label_color = if off_reason.is_some() { disabled } else { ink };
        let ctl: gpui::AnyElement = match row.ctl.clone() {
            Ctl::Radio { key, value, on } => div()
                .id(("set-radio", id as u64))
                .cursor_pointer()
                .text_color(rgb(if on { blue } else { disabled }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_pref(key, serde_json::Value::String(value.to_owned()), window, cx);
                    cx.notify();
                }))
                .child(fonts::icon_el(
                    if on { '\u{ECEF}' } else { '\u{EDD3}' },
                    "○",
                    24.,
                ))
                .into_any_element(),
            Ctl::Toggle { key, on } => div()
                .id(("set-toggle", id as u64))
                .cursor_pointer()
                .relative()
                .w(px(38.))
                .h(px(24.))
                .text_color(rgb(if on { blue } else { disabled }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_pref(key, serde_json::Value::Bool(!on), window, cx);
                    cx.notify();
                }))
                .child(toggle_el(on))
                .into_any_element(),
            Ctl::Select {
                key,
                options,
                current,
            } => {
                let clickable = key.is_some() && !options.is_empty();
                let display = select_display_label(&options, &current);
                let tip_text = if display != current && !current.is_empty() && current != "Mặc định"
                {
                    format!("{display}\n({current})")
                } else {
                    display.clone()
                };
                div()
                    .id(("set-select", id as u64))
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .min_w(px(140.))
                    .max_w(px(270.))
                    .h(px(32.))
                    .px(px(12.))
                    .gap(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(crate::color::border_subtle())
                    .text_size(px(14.))
                    .text_color(rgb(if clickable { ink } else { muted }))
                    .tooltip(move |_, cx| tip(tip_text.clone(), cx))
                    .when(clickable, |el| {
                        let opts = options.clone();
                        let cur = current.clone();
                        el.cursor_pointer().on_click(cx.listener(
                            move |this, e: &gpui::ClickEvent, window, cx| {
                                this.open_menu(
                                    crate::context_menu::MenuKind::SettingSelect {
                                        key,
                                        options: opts.clone(),
                                        current: cur.clone(),
                                    },
                                    e.position(),
                                    window,
                                );
                                cx.notify();
                            },
                        ))
                    })
                    .child(div().flex_1().min_w(px(0.)).truncate().child(display))
                    .child(
                        div()
                            .flex_none()
                            .child(fonts::icon_el('\u{EA8C}', "⌄", 16.)),
                    )
                    .into_any_element()
            }
            Ctl::Info(text) => div()
                .text_size(px(14.))
                .text_color(rgb(muted))
                .child(crate::i18n::t(&text))
                .into_any_element(),
            Ctl::Button(label, action) => div()
                .id(("set-btn", id as u64))
                .px(px(16.))
                .h(px(32.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
                .hover(|st| st.bg(rgb(token_rgb("--N30", "#d6dbe1"))))
                .cursor_pointer()
                .text_size(px(14.))
                .font_family(FAMILY_SEMIBOLD)
                .text_color(rgb(ink))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.run_setting_action(action, cx);
                    cx.notify();
                }))
                .child(crate::i18n::t(&label))
                .into_any_element(),
            Ctl::Off(_) => div()
                .text_size(px(13.))
                .text_color(rgb(disabled))
                .child(crate::i18n::t("Chưa hỗ trợ"))
                .into_any_element(),
            Ctl::MicVolume { percent } => {
                let step = |id: &'static str, label: &'static str, next: u8| {
                    div()
                        .id((id, id_of(id)))
                        .w(px(28.))
                        .h(px(28.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .bg(rgb(token_rgb("--layer-background-subtle", "#ebecf0")))
                        .hover(|st| st.bg(rgb(token_rgb("--N30", "#d6dbe1"))))
                        .text_size(px(16.))
                        .text_color(rgb(ink))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_pref("micVolume", serde_json::json!(next), window, cx);
                            let source = this.prefs.str("micDevice", "");
                            crate::call::push_mic_volume(&source, next);
                            cx.notify();
                        }))
                        .child(label)
                };
                fn id_of(which: &str) -> u64 {
                    if which.ends_with("dec") {
                        1
                    } else {
                        2
                    }
                }
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .child(step("mic-set-dec", "−", percent.saturating_sub(5)))
                    .child(
                        div()
                            .w(px(44.))
                            .flex()
                            .justify_center()
                            .text_size(px(14.))
                            .text_color(rgb(ink))
                            .child(format!("{percent}%")),
                    )
                    .child(step("mic-set-inc", "+", percent.saturating_add(5).min(100)))
                    .into_any_element()
            }
            Ctl::None => div().into_any_element(),
        };
        div()
            .id(("set-row", id as u64))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .px(px(16.))
            .min_h(px(24.))
            .when_some(off_reason, |el, reason| {
                el.tooltip(move |_, cx| tip(reason, cx))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(120.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .line_height(px(21.))
                            .text_color(rgb(label_color))
                            .child(crate::i18n::t(&row.label)),
                    )
                    .when_some(row.desc, |el, d| {
                        el.child(
                            div()
                                .text_size(px(13.))
                                .line_height(px(18.2))
                                .text_color(rgb(muted))
                                .child(crate::media::wrap_ui_note(
                                    &crate::i18n::t(&d),
                                    note_width(&row.ctl),
                                )),
                        )
                    }),
            )
            .child(ctl)
    }

    /// Page model, built from what the live Zalo PC dialog shows.
    fn page_sections(&self, page: SettingsPage) -> Vec<Section> {
        let p = &self.prefs;
        let radio = |key: &'static str, value: &'static str, current: &str, label: &str| SRow {
            label: label.to_owned(),
            desc: None,
            ctl: Ctl::Radio {
                key,
                value,
                on: current == value,
            },
        };
        let toggle = |key: &'static str, default: bool, label: &str, desc: Option<&str>| SRow {
            label: label.to_owned(),
            desc: desc.map(str::to_owned),
            ctl: Ctl::Toggle {
                key,
                on: p.bool(key, default),
            },
        };
        let off = |label: &str, reason: &'static str| SRow {
            label: label.to_owned(),
            desc: None,
            ctl: Ctl::Off(reason),
        };
        let sec = |title: &str, desc: Option<&str>, rows: Vec<SRow>| Section {
            title: title.to_owned(),
            desc: desc.map(str::to_owned),
            rows,
        };
        const NEED_API: &str = "Cần API máy chủ — bản GPUI chưa nối";
        match page {
            SettingsPage::General => {
                let friends = p.str("friendFilter", "all");
                let lang = if p.str("lang", "vi") == "en" { "English" } else { "Tiếng Việt" };
                vec![
                    sec(
                        "Danh bạ",
                        Some("Danh sách bạn bè được hiển thị trong danh bạ"),
                        vec![
                            radio("friendFilter", "all", &friends, "Hiển thị tất cả bạn bè"),
                            radio("friendFilter", "zalo", &friends, "Chỉ hiển thị bạn bè đang sử dụng Zalo"),
                        ],
                    ),
                    sec(
                        "Ngôn ngữ",
                        None,
                        vec![SRow {
                            label: "Thay đổi ngôn ngữ".into(),
                            desc: None,
                            ctl: Ctl::Select {
                                key: Some("langLabel"),
                                options: flat_options(&["Tiếng Việt", "English"]),
                                current: lang.into(),
                            },
                        }],
                    ),
                    sec(
                        "Khởi động & ghi nhớ tài khoản",
                        None,
                        vec![
                            toggle("autostart", zalo_shell::autostart_enabled(), "Khởi động Zalo khi mở máy", None),
                            toggle("rememberLogin", true, "Ghi nhớ tài khoản đăng nhập", None),
                        ],
                    ),
                ]
            }
            SettingsPage::Security => vec![
                sec(
                    "Khóa màn hình Zalo",
                    Some("Khóa màn hình Zalo của bạn bằng Ctrl + L, khi bạn không sử dụng máy tính."),
                    if p.str("lockHash", "").is_empty() {
                        vec![SRow { label: "Khóa màn hình Zalo".into(), desc: Some("Chưa đặt mã khóa".into()), ctl: Ctl::Button("Tạo mã khóa", SAction::LockSet) }]
                    } else {
                        vec![
                            SRow { label: "Đổi mã khóa".into(), desc: Some("Đang bật — Ctrl + L để khóa".into()), ctl: Ctl::Button("Đổi mã", SAction::LockSet) },
                            SRow { label: "Tắt khóa màn hình".into(), desc: None, ctl: Ctl::Button("Tắt", SAction::LockOff) },
                        ]
                    },
                ),
                sec(
                    "Bảo mật 2 lớp",
                    Some("Sau khi bật, bạn sẽ được yêu cầu nhập mã OTP hoặc xác thực từ thiết bị di động sau khi đăng nhập trên thiết bị lạ."),
                    vec![if !self.has_session {
                        off("Bảo mật 2 lớp", NEED_API)
                    } else {
                        match tfa_enabled_from(self.privacy.values.as_ref()) {
                            Some(true) => SRow {
                                label: "Bảo mật 2 lớp".into(),
                                desc: Some("Đang bật — yêu cầu OTP khi đăng nhập trên thiết bị lạ".into()),
                                ctl: Ctl::Button("Tắt", SAction::Toggle2Fa(false)),
                            },
                            Some(false) => SRow {
                                label: "Bảo mật 2 lớp".into(),
                                desc: Some("Đang tắt — bảo vệ tài khoản bằng mã OTP".into()),
                                ctl: Ctl::Button("Bật", SAction::Toggle2Fa(true)),
                            },
                            None => SRow {
                                label: "Bảo mật 2 lớp".into(),
                                desc: Some(if self.privacy.loading {
                                    "Đang kiểm tra trạng thái từ máy chủ…"
                                } else {
                                    "Bấm Bật hoặc Tắt để xác thực OTP 2FA"
                                }.into()),
                                ctl: Ctl::Button("Thiết lập 2FA", SAction::Toggle2Fa(true)),
                            },
                        }
                    }],
                ),
            ],
            SettingsPage::Privacy => self.privacy_sections(),
            // Painted by `sync_page`: two illustrated cards, like the PC dialog.
            SettingsPage::Sync => Vec::new(),
            // Rendered directly by `data_page` in `settings_body`.
            SettingsPage::Data => Vec::new(),
            SettingsPage::Theme => {
                let saved_wall = p.str("chatWallpaper", "");
                let wall_path = p.str("chatWallpaperPath", "");
                let wall = if saved_wall == "file" && !wall_path.is_empty() {
                    "file".to_owned()
                } else {
                    "none".to_owned()
                };
                let file_label = std::path::Path::new(&wall_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|_| !wall_path.is_empty())
                    .unwrap_or("Chưa chọn ảnh")
                    .to_owned();
                vec![sec(
                    "Hình nền chat",
                    None,
                    vec![
                        radio("chatWallpaper", "none", &wall, "Không ảnh nền"),
                        radio("chatWallpaper", "file", &wall, "Ảnh từ máy này"),
                        SRow {
                            label: file_label,
                            desc: Some("Chọn một ảnh trên máy làm nền hội thoại".into()),
                            ctl: Ctl::Button("Chọn ảnh", SAction::PickWallpaper),
                        },
                    ],
                )]
            }
            SettingsPage::Notify => {
                let on = if p.bool("notifyOff", false) { "off" } else { "on" };
                let corner = match p.str("notifyCorner", "tr").as_str() {
                    "tl" | "tr" | "bl" | "br" | "custom" => p.str("notifyCorner", "tr"),
                    _ => "tr".to_owned(),
                };
                let duration_secs = p
                    .json("notifyDuration")
                    .and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                    })
                    .unwrap_or(crate::notice::DEFAULT_DURATION_SECS);
                let duration_key = match duration_secs {
                    3 => "3",
                    10 => "10",
                    15 => "15",
                    _ => "5",
                };
                vec![
                    sec(
                        "Cài đặt thông báo",
                        Some("Nhận được thông báo mỗi khi có tin nhắn mới"),
                        vec![radio("notify", "on", on, "Bật"), radio("notify", "off", on, "Tắt")],
                    ),
                    sec(
                        "Âm thanh thông báo",
                        None,
                        vec![toggle("notifySound", true, "Phát âm thanh khi có tin nhắn & thông báo mới", None)],
                    ),
                    sec(
                        "Thời gian hiển thị thông báo",
                        Some("Tự đóng sau khi thanh tiến trình đếm ngược chạy hết (rê chuột lên thẻ để tạm dừng)."),
                        vec![
                            radio("notifyDuration", "3", duration_key, "3 giây"),
                            radio("notifyDuration", "5", duration_key, "5 giây (mặc định)"),
                            radio("notifyDuration", "10", duration_key, "10 giây"),
                            radio("notifyDuration", "15", duration_key, "15 giây"),
                        ],
                    ),
                    sec(
                        "Vị trí hiển thị thông báo",
                        Some("Giống thông báo Zalo: một thẻ riêng, luôn nổi trên màn hình. Chọn một góc màn hình, hoặc kéo thẻ để đặt chỗ khác."),
                        vec![
                            radio("notifyCorner", "tl", &corner, "Phía trên bên trái"),
                            radio("notifyCorner", "tr", &corner, "Phía trên bên phải"),
                            radio("notifyCorner", "bl", &corner, "Phía dưới bên trái"),
                            radio("notifyCorner", "br", &corner, "Phía dưới bên phải"),
                            radio("notifyCorner", "custom", &corner, "Tùy chỉnh"),
                            SRow {
                                label: "Xem thử vị trí".into(),
                                desc: Some("Hiện một thẻ mẫu trên màn hình. Kéo thẻ để nhớ vị trí.".into()),
                                ctl: Ctl::Button("Xem thử", SAction::PreviewNotice),
                            },
                        ],
                    ),
                ]
            }
            SettingsPage::Messages => {
                let excel = p.str("excelPaste", "ask");
                let dir = self.download_dir();
                vec![
                    sec(
                        "Tin nhắn nhanh",
                        Some("Tạo, chỉnh sửa và quản lý phím tắt cho những tin nhắn thường sử dụng trong hội thoại"),
                        vec![SRow {
                            label: "Quản lý tin nhắn nhanh".into(),
                            desc: Some(format!(
                                "{} mẫu · gợi ý /phím tắt đang {}",
                                crate::quick::load_quick(p).len(),
                                if crate::quick::quick_enabled(p) { "bật" } else { "tắt" }
                            )),
                            ctl: Ctl::Button("Mở", SAction::QuickMessages),
                        }],
                    ),
                    sec(
                        "Trả lời tự động",
                        Some("Thiết lập tin trả lời tự động để thông báo cho người khác khi bạn không thể phản hồi"),
                        vec![SRow { label: "Thiết lập".into(), desc: None, ctl: Ctl::Button("Mở", SAction::AutoReply) }],
                    ),
                    sec(
                        "Thiết lập ẩn trò chuyện",
                        Some("Đặt mã PIN cho các cuộc trò chuyện riêng tư để tránh người khác nhìn thấy trên máy của bạn"),
                        if self.hidden_srv.pin_md5.is_empty() {
                            vec![SRow { label: "Mã PIN".into(), desc: Some("Chưa đặt — đặt khi ẩn trò chuyện đầu tiên".into()), ctl: Ctl::Button("Đặt mã PIN", SAction::HiddenPin) }]
                        } else if self.hidden_srv.unlocked {
                            vec![
                                SRow { label: "Mã PIN".into(), desc: Some(format!("Đã đặt · {} trò chuyện ẩn (đang mở)", self.hidden.len())), ctl: Ctl::Button("Đổi mã PIN", SAction::HiddenPin) },
                                SRow { label: "Ẩn lại trò chuyện".into(), desc: Some("Ẩn các trò chuyện riêng tư khỏi danh sách".into()), ctl: Ctl::Button("Ẩn lại", SAction::LockHidden) },
                            ]
                        } else {
                            vec![
                                SRow { label: "Mã PIN".into(), desc: Some(format!("Đã đặt · {} trò chuyện ẩn", self.hidden.len())), ctl: Ctl::Button("Đổi mã PIN", SAction::HiddenPin) },
                                SRow { label: "Hiện trò chuyện ẩn".into(), desc: Some("Nhập mã PIN để xem trong lần dùng này".into()), ctl: Ctl::Button("Nhập PIN", SAction::ShowHidden) },
                            ]
                        },
                    ),
                    sec(
                        "Chia mục Ưu tiên và Khác",
                        Some("Tách riêng trò chuyện không ưu tiên và chuyển sang mục Khác để tập trung hơn"),
                        vec![toggle("split", true, "Chia mục Ưu tiên", None)],
                    ),
                    sec(
                        "File được lưu tại thư mục:",
                        None,
                        vec![SRow {
                            label: dir.display().to_string(),
                            desc: None,
                            ctl: Ctl::Button("Thay đổi", SAction::PickDownloadDir),
                        }],
                    ),
                    sec(
                        "Hiển thị nội dung file Excel",
                        Some("Định dạng khi sao chép file Excel sau đó dán vào Zalo"),
                        vec![
                            radio("excelPaste", "image", &excel, "Dạng hình"),
                            radio("excelPaste", "text", &excel, "Dạng chữ"),
                            radio("excelPaste", "ask", &excel, "Luôn hỏi"),
                        ],
                    ),
                    sec(
                        "Cài đặt khác",
                        None,
                        vec![
                            toggle("dblReply", true, "Bấm đúp vào vùng cạnh tin nhắn để trả lời", None),
                            toggle("autoDownload", true, "Tự động tải trước Ảnh/File/Video/Tin nhắn thoại dưới 30MB", None),
                            toggle("fileThumb", true, "Hiển thị hình thu nhỏ (Thumbnail) của File", None),
                            SRow {
                                label: "Bạn sẽ thấy bạn bè bạn đang soạn tin nhắn".into(),
                                desc: None,
                                ctl: Ctl::Toggle { key: "typing", on: !p.bool("hideTyping", false) },
                            },
                            toggle("muteVideo", false, "Tắt tiếng video trong cửa sổ chat", None),
                        ],
                    ),
                ]
            }
            SettingsPage::Call => {
                let c = self.settings_cache.clone().unwrap_or_default();
                let pick_pulse = |list: &[(String, String)], key: &str| {
                    let cur = p.str(key, "");
                    let current = if cur.is_empty() || cur == "Mặc định" {
                        String::new()
                    } else {
                        cur
                    };
                    let mut options = vec![(String::new(), "Mặc định".to_owned())];
                    options.extend(list.iter().cloned());
                    (options, current)
                };
                let pick_flat = |list: &[String], key: &str| {
                    let cur = p.str(key, "Mặc định");
                    let mut options = vec![("Mặc định".to_owned(), "Mặc định".to_owned())];
                    options.extend(list.iter().map(|s| (s.clone(), s.clone())));
                    (options, cur)
                };
                let (mics, mic) = pick_pulse(&c.sources, "micDevice");
                let (spks, spk) = pick_pulse(&c.sinks, "speakerDevice");
                let (cams, cam) = pick_flat(&c.cameras, "cameraDevice");
                vec![
                    sec(
                        "Thông báo",
                        Some("Khi tắt, tất cả cuộc gọi đến sẽ không hiển thị trên Zalo PC. Tuy nhiên bạn vẫn nhận thông báo cuộc gọi đến trên ứng dụng Zalo điện thoại."),
                        vec![toggle("incomingCall", true, "Thông báo cuộc gọi đến trên Zalo PC", None)],
                    ),
                    sec(
                        "Đầu vào âm thanh",
                        None,
                        vec![
                            SRow { label: "Micro".into(), desc: None, ctl: Ctl::Select { key: Some("micDevice"), options: mics, current: mic } },
                            SRow {
                                label: "Âm lượng micro".into(),
                                desc: Some("100% là mức đầy đủ (0 dB). Micro đang nhỏ thì tăng lên.".into()),
                                ctl: Ctl::MicVolume {
                                    percent: crate::call::mic_percent_from_json(p.json("micVolume").as_ref()),
                                },
                            },
                            toggle(
                                "callAec",
                                true,
                                "Khử vọng âm (AEC)",
                                Some("Loại bỏ tiếng vọng từ loa khi gọi không dùng tai nghe"),
                            ),
                            toggle(
                                "callNoiseSuppression",
                                true,
                                "Lọc tiếng ồn nền (AI)",
                                Some("Giảm tiếng quạt, gõ phím và tạp âm môi trường bằng RNNoise"),
                            ),
                            toggle("needMic", false, "Không nhận cuộc gọi trên thiết bị này khi thiếu micro", None),
                        ],
                    ),
                    sec(
                        "Loa/Tai nghe",
                        None,
                        vec![
                            SRow { label: "Loa".into(), desc: None, ctl: Ctl::Select { key: Some("speakerDevice"), options: spks, current: spk } },
                            SRow { label: "Âm thanh đầu ra".into(), desc: Some("Phát thử chuông qua loa đang chọn".into()), ctl: Ctl::Button("Kiểm tra loa", SAction::TestSpeaker) },
                            toggle("needSpeaker", false, "Không nhận cuộc gọi trên thiết bị này khi thiếu loa hoặc tai nghe", None),
                        ],
                    ),
                    sec(
                        "Camera",
                        None,
                        vec![SRow {
                            label: "Camera".into(),
                            desc: None,
                            ctl: Ctl::Select { key: Some("cameraDevice"), options: cams, current: cam },
                        }],
                    ),
                ]
            }
            SettingsPage::Utilities => {
                let (with, without) = self.shot_triggers();
                vec![
                    sec(
                        "Phím tắt chụp màn hình",
                        Some(&self.shortcut_portal_status()),
                        vec![
                            SRow {
                                label: "Chụp màn hình kèm với cửa sổ Zalo".into(),
                                desc: Some(crate::shortcuts::format_trigger(&with)),
                                ctl: Ctl::Button("Đổi phím", SAction::CycleShotWith),
                            },
                            SRow {
                                label: "Chụp màn hình KHÔNG kèm cửa sổ Zalo".into(),
                                desc: Some(crate::shortcuts::format_trigger(&without)),
                                ctl: Ctl::Button("Đổi phím", SAction::CycleShotWithout),
                            },
                        ],
                    ),
                    sec(
                        "Gợi ý Sticker",
                        None,
                        vec![toggle("stickerSuggest", true, "Hiện gợi ý Sticker phù hợp với nội dung tin nhắn đang soạn", None)],
                    ),
                    sec(
                        "Gợi ý @",
                        None,
                        vec![
                            toggle("mentionSuggest", true, "Gợi ý nhắc tên", Some("Gợi ý nhắc tên theo nội dung đang soạn")),
                            toggle("gifSearch", true, "Tìm kiếm GIF", Some("Gõ từ khóa để tìm GIF")),
                            toggle("stickerSearch", true, "Tìm sticker", Some("Gõ từ khóa để tìm Sticker")),
                        ],
                    ),
                ]
            }
            SettingsPage::Advanced => {
                let tone = |key: &'static str, title: &str, desc: &str, pick: SAction, preview: SAction, reset: SAction| {
                    let saved = p.str(key, "");
                    let shown = if saved.is_empty() {
                        "Mặc định của Zalo".to_owned()
                    } else {
                        saved
                    };
                    vec![
                        SRow {
                            label: title.to_owned(),
                            desc: Some(format!("{desc} {shown}")),
                            ctl: Ctl::Button("Chọn tệp", pick),
                        },
                        SRow {
                            label: format!("Nghe thử {title}"),
                            desc: None,
                            ctl: Ctl::Button("Nghe thử", preview),
                        },
                        SRow {
                            label: format!("Đặt lại {title}"),
                            desc: None,
                            ctl: Ctl::Button("Mặc định", reset),
                        },
                    ]
                };
                let mut rows = tone(
                    "linuxRingtone",
                    "Nhạc chuông",
                    "Phát khi có cuộc gọi đến.",
                    SAction::PickCallSound("linuxRingtone"),
                    SAction::PreviewCallSound("linuxRingtone"),
                    SAction::ResetCallSound("linuxRingtone"),
                );
                rows.extend(tone(
                    "linuxRingback",
                    "Nhạc chờ",
                    "Phát trên máy này trong lúc chờ người nhận.",
                    SAction::PickCallSound("linuxRingback"),
                    SAction::PreviewCallSound("linuxRingback"),
                    SAction::ResetCallSound("linuxRingback"),
                ));
                vec![
                    sec(
                        "Âm thanh cuộc gọi",
                        Some("MP3, WAV, OGG, M4A, AAC, FLAC, OPUS, WEBM. Đường dẫn tuyệt đối. Áp dụng từ lần phát tiếp theo."),
                        rows,
                    ),
                    sec(
                        "Chống nhìn trộm",
                        Some("Che nội dung tin nhắn trên máy này. Đưa chuột vào tin để đọc."),
                        vec![toggle("peekGuard", false, "Chống nhìn trộm tin nhắn", None)],
                    ),
                    sec(
                        "Ẩn trạng thái",
                        Some("Khi bật, máy này không gửi trạng thái tương ứng cho người khác."),
                        vec![
                            toggle("hideTyping", false, "Ẩn trạng thái Đang soạn tin", None),
                            toggle("hideDelivered", false, "Ẩn trạng thái Đã nhận", None),
                            toggle("hideSeen", false, "Ẩn trạng thái Đã xem", None),
                        ],
                    ),
                    {
                        let tray_hint = self
                            .settings_cache
                            .as_ref()
                            .map(|c| c.tray_hint.clone())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| {
                                zalo_shell::format_shell_hint(
                                    zalo_shell::current_desktop(),
                                    zalo_shell::tray_backend(),
                                    false,
                                    false,
                                )
                            });
                        sec(
                            "Khay hệ thống & Chạy nền",
                            Some(&tray_hint),
                            vec![SRow {
                                label: "Cho phép chạy nền qua Portal (GNOME / Flatpak)".into(),
                                desc: Some("Giúp hệ thống quản lý Zalo chạy nền an toàn khi không có khay.".into()),
                                ctl: Ctl::Button("Cấp quyền", SAction::PortalBackground),
                            }],
                        )
                    },
                ]
            }
        }
    }

    /// Write one setting and apply its side effect.
    pub(crate) fn set_pref(
        &mut self,
        key: &str,
        value: serde_json::Value,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(srv_key) = key.strip_prefix("srv:") {
            self.apply_privacy_pref(srv_key, &value, cx);
            return;
        }
        match key {
            "theme" => {
                let mode = value.as_str().unwrap_or("light");
                self.prefs
                    .set("followSystem", serde_json::Value::Bool(mode == "system"));
                self.prefs
                    .set("dark", serde_json::Value::Bool(mode == "dark"));
                self.apply_theme(window);
            }
            "notify" => {
                let off = value.as_str() == Some("off");
                self.prefs.set("notifyOff", serde_json::Value::Bool(off));
                zalo_shell::set_tray_muted(off);
            }
            "langLabel" => {
                let en = value.as_str() == Some("English");
                self.prefs.set(
                    "lang",
                    serde_json::Value::String(if en { "en" } else { "vi" }.into()),
                );
                self.english = en;
                crate::i18n::set_english(en);
                self.set_note(if en {
                    "Language: English"
                } else {
                    "Đã chuyển sang Tiếng Việt"
                });
            }
            "typing" => {
                self.prefs.set(
                    "hideTyping",
                    serde_json::Value::Bool(!value.as_bool().unwrap_or(true)),
                );
            }
            "friendFilter" => {
                self.show_all_friends = value.as_str() != Some("zalo");
                self.prefs.set(key, value);
            }
            "autostart" => {
                let on = value.as_bool().unwrap_or(false);
                match zalo_shell::set_autostart_for(zalo_shell::GPUI_APP_ID, on) {
                    Ok(()) => self.prefs.set(key, value),
                    Err(err) => self.set_note(format!("Không ghi được autostart: {err}")),
                }
            }
            "split" => {
                self.prefs.set(key, value);
                self.other_tab = false;
            }
            "chatWallpaper" => {
                self.prefs
                    .set("avatarWallpaper", serde_json::Value::Bool(false));
                if value.as_str() == Some("file") {
                    let path = self.prefs.str("chatWallpaperPath", "");
                    if path.is_empty() || !std::path::Path::new(&path).is_file() {
                        self.prefs
                            .set(key, serde_json::Value::String("none".into()));
                        self.run_setting_action(SAction::PickWallpaper, cx);
                    } else {
                        self.prefs.set(key, value);
                    }
                } else {
                    self.prefs
                        .set(key, serde_json::Value::String("none".into()));
                }
            }
            "notifyDuration" => {
                let secs = value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
                    .unwrap_or(crate::notice::DEFAULT_DURATION_SECS);
                let clamped = crate::notice::clamp_duration_secs(secs);
                self.prefs.set(key, serde_json::json!(clamped));
            }
            "notifyCorner" => {
                let corner = match value.as_str() {
                    Some(corner @ ("tl" | "tr" | "bl" | "br" | "custom")) => corner.to_owned(),
                    _ => "tr".to_owned(),
                };
                self.prefs
                    .set(key, serde_json::Value::String(corner.clone()));
                let x = self
                    .prefs
                    .json("notifyX")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32;
                let y = self
                    .prefs
                    .json("notifyY")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32;
                crate::notice::move_if_open(cx, window.scale_factor(), &corner, x, y);
            }
            _ => self.prefs.set(key, value),
        }
    }

    /// Light / dark tokens from the saved theme (`followSystem` reads the OS).
    pub(crate) fn apply_theme(&mut self, window: &mut gpui::Window) {
        // Dev hook for screenshots: ZALO_GPUI_THEME=dark|light (not saved).
        let forced = std::env::var("ZALO_GPUI_THEME").ok();
        let dark = if let Some(theme) = forced {
            theme == "dark"
        } else if self.prefs.bool("followSystem", false) {
            matches!(
                window.appearance(),
                gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
            )
        } else {
            self.prefs.bool("dark", false)
        };
        crate::color::set_dark(dark);
        window.refresh();
    }

    fn run_setting_action(&mut self, action: SAction, cx: &mut Context<Self>) {
        use crate::privacy::PinMode;
        match action {
            SAction::LockSet => self.open_pin(PinMode::LockSet, cx),
            SAction::LockOff => self.open_pin(PinMode::LockOff, cx),
            // Changing the PIN needs the old one first (else anyone could
            // reset it and read the hidden chats).
            SAction::HiddenPin => {
                if self.hidden_srv.pin_md5.is_empty() || self.hidden_srv.unlocked {
                    self.open_pin(PinMode::SetPin { conv: None }, cx);
                } else {
                    self.open_pin(PinMode::Unlock { conv: None }, cx);
                    self.set_note("Nhập mã PIN hiện tại trước khi đổi");
                }
            }
            SAction::ShowHidden => self.open_pin(PinMode::Unlock { conv: None }, cx),
            SAction::LockHidden => self.lock_hidden_chats(),
            SAction::QuickMessages => self.open_quick_messages(cx),
            SAction::AutoReply => self.open_auto_reply(cx),
            SAction::ClearCache => {
                let n = crate::media::media_root()
                    .map(|r| r.join("thumbs"))
                    .and_then(|d| std::fs::read_dir(d).ok())
                    .map(|it| {
                        it.flatten()
                            .filter(|e| {
                                e.path().is_file() && std::fs::remove_file(e.path()).is_ok()
                            })
                            .count()
                    })
                    .unwrap_or(0);
                self.media_bitmaps.clear();
                self.media_pending.clear();
                self.media_failed.clear();
                if let Some(id) = self.selected {
                    self.warm_media(id);
                }
                self.refresh_settings_cache(SettingsPage::Data);
                self.set_note(format!("Đã xóa {n} tệp bộ nhớ đệm ảnh"));
            }
            SAction::OpenMedia => {
                if let Some(dir) = crate::media::media_root() {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(dir.join("photos"))
                        .spawn();
                }
            }
            SAction::MoveData => match zalo_shell::pick_folder("Chọn vị trí lưu trữ dữ liệu Zalo")
            {
                Ok(Some(dir)) => {
                    let new_root = dir.join("zalo-rust-media");
                    let old_root = crate::media::media_root();
                    match old_root.as_deref().map(|old| move_media(old, &new_root)) {
                        Some(Ok(n)) => {
                            crate::media::set_media_root(Some(new_root.clone()));
                            self.prefs.set(
                                "dataDir",
                                serde_json::Value::String(new_root.display().to_string()),
                            );
                            self.refresh_settings_cache(SettingsPage::Data);
                            self.set_note(format!("Đã chuyển {n} tệp sang {}", new_root.display()));
                        }
                        Some(Err(e)) => self.set_note(format!("Không chuyển được dữ liệu: {e}")),
                        None => {}
                    }
                }
                Ok(None) => {}
                Err(err) => self.set_note(err.to_string()),
            },
            SAction::PickDownloadDir => match zalo_shell::pick_folder("Chọn thư mục lưu file")
            {
                Ok(Some(dir)) => {
                    self.prefs.set(
                        "downloadDir",
                        serde_json::Value::String(dir.display().to_string()),
                    );
                }
                Ok(None) => {}
                Err(err) => self.set_note(err.to_string()),
            },
            SAction::ReloadPrivacy => {
                self.privacy.values = None;
                self.privacy.err = None;
                self.ensure_privacy_loaded(cx);
            }
            SAction::BackupDecrypt => self.open_backup_decrypt(cx),
            SAction::BlockedList => {
                self.privacy.blocked_open = !self.privacy.blocked_open;
                if self.privacy.blocked_open && self.privacy.blocked.is_none() {
                    self.load_block_list(cx);
                }
            }
            SAction::CycleShotWith => self.cycle_shot_trigger(0),
            SAction::CycleShotWithout => self.cycle_shot_trigger(1),
            SAction::PortalBackground => {
                let autostart_on = self
                    .prefs
                    .bool("autostart", zalo_shell::autostart_enabled());
                zalo_shell::request_background_async(zalo_shell::GPUI_APP_ID, autostart_on, true);
                self.set_note("Đã gửi yêu cầu chạy nền tới hệ thống (XDG Portal Background)");
            }
            SAction::TestSpeaker => {
                let sink = self.prefs.str("speakerDevice", "");
                crate::call::play_speaker_test((!sink.is_empty()).then_some(sink.as_str()));
                self.set_note("Đang phát thử âm thanh qua loa đã chọn");
            }
            SAction::Toggle2Fa(turn_on) => self.open_two_factor_dialog(turn_on, cx),
            SAction::PickCallSound(key) => {
                let title = if key == "linuxRingtone" {
                    "Chọn nhạc chuông"
                } else {
                    "Chọn nhạc chờ"
                };
                match zalo_shell::pick_file(title) {
                    Ok(Some(path)) => {
                        match crate::call::validate_call_sound(&path.display().to_string()) {
                            Ok(clean) => {
                                self.prefs.set(key, serde_json::Value::String(clean));
                                self.set_note("Đã lưu âm thanh. Áp dụng từ lần phát tiếp theo.");
                            }
                            Err(err) => self.set_note(err),
                        }
                    }
                    Ok(None) => {}
                    Err(err) => self.set_note(err.to_string()),
                }
            }
            SAction::PreviewCallSound(key) => {
                let kind = if key == "linuxRingtone" {
                    "ringtone"
                } else {
                    "ringback"
                };
                let custom = self.prefs.str(key, "");
                match crate::call::resolve_call_sound(kind, &custom) {
                    Some(path) => {
                        crate::call::preview_call_sound(&path);
                        self.set_note("Đang nghe thử");
                    }
                    None => self.set_note("Không tìm thấy tệp âm thanh"),
                }
            }
            SAction::ResetCallSound(key) => {
                self.prefs
                    .set(key, serde_json::Value::String(String::new()));
                self.set_note("Đã dùng âm thanh mặc định của Zalo");
            }
            SAction::PickWallpaper => match zalo_shell::pick_image("Chọn ảnh nền hội thoại")
            {
                Ok(Some(path)) => {
                    if crate::media::decode_thumb(&path, 64).is_none() {
                        self.set_note("Không đọc được ảnh này");
                    } else {
                        self.prefs.set(
                            "chatWallpaperPath",
                            serde_json::Value::String(path.display().to_string()),
                        );
                        self.prefs
                            .set("chatWallpaper", serde_json::Value::String("file".into()));
                        self.prefs
                            .set("avatarWallpaper", serde_json::Value::Bool(false));
                        self.wall_image.borrow_mut().take();
                        self.set_note("Đã đặt ảnh nền hội thoại");
                    }
                }
                Ok(None) => {}
                Err(err) => self.set_note(err.to_string()),
            },
            SAction::PreviewNotice => {
                self.notice_thread = None;
                self.show_msg_notice(
                    "Thông báo mẫu".into(),
                    "Kéo thẻ này để đặt vị trí trên màn hình.".into(),
                    "Z".into(),
                    0x0068ff,
                    None,
                    Vec::new(),
                );
            }
        }
    }

    /// Download folder: saved choice, else `XDG_DOWNLOAD_DIR`, else ~/Downloads.
    pub(crate) fn download_dir(&self) -> std::path::PathBuf {
        let saved = self.prefs.str("downloadDir", "");
        if !saved.is_empty() {
            return saved.into();
        }
        std::env::var_os("XDG_DOWNLOAD_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Downloads"))
            })
            .unwrap_or_else(|| "/tmp".into())
    }

    /// Disk sizes / audio devices for the page being opened.
    pub(crate) fn refresh_settings_cache(&mut self, page: SettingsPage) {
        let mut c = self.settings_cache.clone().unwrap_or_default();
        match page {
            SettingsPage::General => {
                c.tray_hint = zalo_shell::desktop_shell_status().hint;
            }
            SettingsPage::Data => {
                c.media = crate::media::photo_cache_dir()
                    .map(|d| dir_size(&d))
                    .unwrap_or(0);
                c.cache = crate::media::media_root()
                    .map(|r| dir_size(&r.join("thumbs")))
                    .unwrap_or(0)
                    + zalo_core::XdgDirs::from_env()
                        .ok()
                        .map(|x| {
                            let p = x.cache_home().join("zalo-rust").join("gpui-profiles.json");
                            std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
                        })
                        .unwrap_or(0);
                if let Some((mount, total, used, avail)) =
                    crate::media::media_root().as_deref().and_then(disk_info)
                {
                    c.mount = mount;
                    c.fs_total = total;
                    c.fs_used = used;
                    c.free = format!("{} trống", fmt_size(avail));
                }
                c.updated = {
                    let now = crate::data::now_ms();
                    let (y, m, d) = crate::data::vn_ymd(now);
                    format!("{} - {d:02}/{m:02}/{y}", crate::data::vn_hm(now))
                };
                c.system = self
                    .session_dir
                    .as_ref()
                    .and_then(|d| std::fs::metadata(d.join("store.db")).ok())
                    .map(|m| m.len())
                    .unwrap_or(0);
            }
            SettingsPage::Call => {
                c.sources = crate::call::pulse_devices("sources");
                c.sinks = crate::call::pulse_devices("sinks");
                c.cameras = crate::call_camera::camera_option_labels();
            }
            _ => {}
        }
        self.settings_cache = Some(c);
    }
}

/// One settings row control.
#[derive(Clone, Debug)]
enum Ctl {
    Radio {
        key: &'static str,
        value: &'static str,
        on: bool,
    },
    Toggle {
        key: &'static str,
        on: bool,
    },
    Select {
        key: Option<&'static str>,
        options: Vec<(String, String)>,
        current: String,
    },
    Info(String),
    Button(&'static str, SAction),
    /// Microphone level 0..=100. Steps of 5 write `micVolume` and Pulse.
    MicVolume {
        percent: u8,
    },
    Off(&'static str),
    #[allow(dead_code)]
    None,
}

#[derive(Clone, Copy, Debug)]
enum SAction {
    LockSet,
    LockOff,
    HiddenPin,
    ShowHidden,
    LockHidden,
    QuickMessages,
    AutoReply,
    /// Kept so the decrypt dialog in `account.rs` still has a call site.
    /// The PC settings page does not show this row.
    #[allow(dead_code)]
    BackupDecrypt,
    ClearCache,
    PickDownloadDir,
    MoveData,
    OpenMedia,
    ReloadPrivacy,
    BlockedList,
    CycleShotWith,
    CycleShotWithout,
    PortalBackground,
    TestSpeaker,
    Toggle2Fa(bool),
    PickCallSound(&'static str),
    PreviewCallSound(&'static str),
    ResetCallSound(&'static str),
    PickWallpaper,
    PreviewNotice,
}

/// Pixel width for a row note, leaving room for the control on the right.
fn note_width(ctl: &Ctl) -> f32 {
    match ctl {
        Ctl::Select { .. } => 230.0,
        Ctl::Button(_, _) => 340.0,
        Ctl::MicVolume { .. } => 300.0,
        Ctl::Info(_) => 400.0,
        _ => 460.0,
    }
}

/// Account privacy state synced with `/api/setting/me`, `/api/setting/update`,
/// and `/api/friend/blocklistv2`.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrivacyState {
    pub values: Option<HashMap<String, i64>>,
    pub loading: bool,
    pub err: Option<String>,
    pub busy: HashSet<String>,
    pub blocked_open: bool,
    pub blocked: Option<Vec<(String, String)>>,
    pub blocked_busy: bool,
    pub blocked_err: Option<String>,
    pub unblocking: HashSet<String>,
}

const BIRTHDAY_OPTS: [(i64, &str); 3] = [
    (0, "Không hiện"),
    (1, "Hiện đầy đủ ngày, tháng, năm"),
    (2, "Chỉ hiện ngày, tháng"),
];

const MSG_OPTS: [(i64, &str); 2] = [(1, "Tất cả mọi người"), (2, "Bạn bè")];

const CALL_OPTS: [(i64, &str); 3] = [
    (2, "Bạn bè"),
    (4, "Bạn bè và người lạ từng liên hệ"),
    (3, "Tất cả mọi người"),
];

fn option_label(opts: &[(i64, &str)], code: i64) -> String {
    opts.iter()
        .find(|(c, _)| *c == code)
        .unwrap_or(&opts[0])
        .1
        .to_owned()
}

fn option_code(opts: &[(i64, &str)], label: &str) -> i64 {
    opts.iter().find(|(_, l)| *l == label).unwrap_or(&opts[0]).0
}

/// Returns live 2FA enabled state from `/api/setting/me` values when loaded.
#[must_use]
pub(crate) fn tfa_enabled_from(values: Option<&HashMap<String, i64>>) -> Option<bool> {
    values.map(|m| m.get("setting_2FA_status").copied().unwrap_or(0) != 0)
}

/// Extracts integer/bool privacy values from `/api/setting/me` JSON.
#[must_use]
pub(crate) fn privacy_from(value: &serde_json::Value) -> HashMap<String, i64> {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let mut out = HashMap::new();
    if let Some(map) = root.as_object() {
        for (key, v) in map {
            let n = v
                .as_i64()
                .or_else(|| v.as_bool().map(i64::from))
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()));
            if let Some(n) = n {
                out.insert(key.clone(), n);
            }
        }
    }
    out
}

/// Extracts `(uid, name)` rows and `hasMore` from `/api/friend/blocklistv2`.
#[must_use]
pub(crate) fn block_list_from(value: &serde_json::Value) -> (Vec<(String, String)>, bool) {
    let root = value.get("data").unwrap_or(value);
    let text = |v: &serde_json::Value, keys: &[&str]| {
        keys.iter()
            .find_map(|k| match v.get(*k) {
                Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                    Some(s.trim().to_owned())
                }
                Some(serde_json::Value::Number(n)) => Some(n.to_string()),
                _ => None,
            })
            .unwrap_or_default()
    };
    let people = root
        .get("profiles")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|p| {
                    let uid = text(p, &["userId", "uid", "id"]);
                    (!uid.is_empty()).then(|| {
                        let name =
                            text(p, &["displayName", "zaloName", "zalo_name", "display_name"]);
                        (uid, name)
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let more = ["hasMore", "has_more"].iter().any(|k| match root.get(*k) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    });
    (people, more)
}

impl ShellView {
    /// Builds `SettingsPage::Privacy` sections backed by `/api/setting/me` & `blocklistv2`.
    fn privacy_sections(&self) -> Vec<Section> {
        let sec = |title: &str, desc: Option<&str>, rows: Vec<SRow>| Section {
            title: title.to_owned(),
            desc: desc.map(str::to_owned),
            rows,
        };
        if !self.has_session && self.privacy.values.is_none() {
            return vec![sec(
                "Quyền riêng tư",
                None,
                vec![SRow {
                    label: "Trạng thái".into(),
                    desc: Some("Cần đăng nhập để xem và thay đổi quyền riêng tư".into()),
                    ctl: Ctl::Info("Chưa đăng nhập".into()),
                }],
            )];
        }
        let Some(map) = &self.privacy.values else {
            let row = if let Some(err) = &self.privacy.err {
                SRow {
                    label: "Không tải được cài đặt riêng tư".into(),
                    desc: Some(err.clone()),
                    ctl: Ctl::Button("Thử lại", SAction::ReloadPrivacy),
                }
            } else {
                SRow {
                    label: "Đang tải cài đặt riêng tư từ máy chủ…".into(),
                    desc: None,
                    ctl: Ctl::Info("…".into()),
                }
            };
            return vec![sec("Quyền riêng tư", None, vec![row])];
        };
        let get = |k: &str, default: i64| map.get(k).copied().unwrap_or(default);
        let select_row =
            |label: &str, key: &'static str, opts: &[(i64, &str)], default: i64| SRow {
                label: label.to_owned(),
                desc: None,
                ctl: Ctl::Select {
                    key: Some(key),
                    options: opts
                        .iter()
                        .map(|(_, s)| ((*s).to_owned(), (*s).to_owned()))
                        .collect(),
                    current: option_label(
                        opts,
                        get(key.strip_prefix("srv:").unwrap_or(key), default),
                    ),
                },
            };
        let toggle_row = |label: &str, srv_key: &'static str, raw_key: &str, default: i64| SRow {
            label: label.to_owned(),
            desc: None,
            ctl: Ctl::Toggle {
                key: srv_key,
                on: get(raw_key, default) != 0,
            },
        };
        let blocked_desc = self
            .privacy
            .blocked
            .as_ref()
            .map(|list| format!("{} người đang bị chặn", list.len()));
        vec![
            sec(
                "Cá nhân",
                None,
                vec![
                    select_row("Hiện ngày sinh", "srv:view_birthday", &BIRTHDAY_OPTS, 0),
                    toggle_row(
                        "Hiển thị trạng thái truy cập",
                        "srv:show_online_status",
                        "show_online_status",
                        1,
                    ),
                ],
            ),
            sec(
                "Tin nhắn và cuộc gọi",
                None,
                vec![
                    toggle_row(
                        "Hiện trạng thái \"Đã xem\"",
                        "srv:display_seen_status",
                        "display_seen_status",
                        0,
                    ),
                    select_row("Cho phép nhắn tin", "srv:receive_message", &MSG_OPTS, 1),
                    select_row(
                        "Cho phép gọi điện",
                        "srv:accept_stranger_call",
                        &CALL_OPTS,
                        2,
                    ),
                ],
            ),
            sec(
                "Chặn tin nhắn",
                None,
                vec![SRow {
                    label: "Danh sách chặn".into(),
                    desc: blocked_desc,
                    ctl: Ctl::Button(
                        if self.privacy.blocked_open {
                            "Ẩn"
                        } else {
                            "Xem"
                        },
                        SAction::BlockedList,
                    ),
                }],
            ),
            sec(
                "Nguồn tìm kiếm",
                None,
                vec![toggle_row(
                    "Cho phép người lạ tìm thấy và kết bạn qua số điện thoại",
                    "srv:add_friend_via_phone",
                    "add_friend_via_phone",
                    1,
                )],
            ),
            sec(
                "Cho phép người lạ kết bạn",
                None,
                vec![
                    toggle_row(
                        "Mã QR của tôi",
                        "srv:add_friend_via_qr",
                        "add_friend_via_qr",
                        1,
                    ),
                    toggle_row(
                        "Nhóm chung",
                        "srv:add_friend_via_group",
                        "add_friend_via_group",
                        1,
                    ),
                    toggle_row(
                        "Danh thiếp Zalo",
                        "srv:add_friend_via_contact",
                        "add_friend_via_contact",
                        1,
                    ),
                    toggle_row(
                        "Gợi ý \"Có thể bạn quen\"",
                        "srv:display_on_recommend_friend",
                        "display_on_recommend_friend",
                        0,
                    ),
                ],
            ),
        ]
    }

    fn blocked_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let paper = token_rgb("--layer-background", "#ffffff");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        div()
            .id("privacy-blocked-panel")
            .mx(px(12.))
            .mb(px(24.))
            .p(px(14.))
            .rounded(px(8.))
            .bg(rgb(paper))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(14.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child("Người bị chặn"),
            )
            .child(
                if self.privacy.blocked_busy && self.privacy.blocked.is_none() {
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .child("Đang tải danh sách chặn…")
                        .into_any_element()
                } else if let Some(err) = &self.privacy.blocked_err {
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .child(format!("Không tải được: {err}"))
                        .into_any_element()
                } else if let Some(list) = &self.privacy.blocked {
                    if list.is_empty() {
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(muted))
                            .child("Bạn chưa chặn ai.")
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .children(list.iter().enumerate().map(|(i, (uid, name))| {
                                let uid_owned = uid.clone();
                                let busy = self.privacy.unblocking.contains(uid);
                                let display = if name.is_empty() {
                                    uid.clone()
                                } else {
                                    name.clone()
                                };
                                div()
                                    .id(("blk-row", i as u64))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .py(px(4.))
                                    .child(
                                        div()
                                            .text_size(px(13.5))
                                            .text_color(rgb(ink))
                                            .truncate()
                                            .child(display),
                                    )
                                    .child(
                                        div()
                                            .id(("blk-unblock", i as u64))
                                            .px(px(10.))
                                            .py(px(4.))
                                            .rounded(px(4.))
                                            .bg(rgb(hover))
                                            .text_size(px(12.5))
                                            .text_color(rgb(ink))
                                            .when(!busy, |d| d.cursor_pointer())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.unblock_uid(uid_owned.clone(), cx);
                                                cx.notify();
                                            }))
                                            .child(if busy { "…" } else { "Bỏ chặn" }),
                                    )
                            }))
                            .into_any_element()
                    }
                } else {
                    div().into_any_element()
                },
            )
    }

    /// Loads `/api/setting/me` once per session so the online dot can honor
    /// `show_online_status` before the Privacy page is opened.
    pub(crate) fn ensure_privacy_loaded(&mut self, cx: &mut Context<Self>) {
        if cfg!(test)
            || !self.has_session
            || self.privacy.values.is_some()
            || self.privacy.loading
            || self.privacy.err.is_some()
        {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        self.privacy.loading = true;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let v = api.get_settings().await.map_err(|e| e.to_string())?;
                        Ok::<_, String>(privacy_from(&v))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.privacy.loading = false;
                match res {
                    Ok(map) => {
                        view.privacy.err = None;
                        view.privacy.values = Some(map);
                    }
                    Err(err) => view.privacy.err = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_privacy_pref(
        &mut self,
        srv_key: &str,
        value: &serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let code = match srv_key {
            "view_birthday" => option_code(&BIRTHDAY_OPTS, value.as_str().unwrap_or("")),
            "receive_message" => option_code(&MSG_OPTS, value.as_str().unwrap_or("")),
            "accept_stranger_call" => option_code(&CALL_OPTS, value.as_str().unwrap_or("")),
            _ => i64::from(value.as_bool().unwrap_or(false)),
        };
        let key_static: Option<&'static str> = match srv_key {
            "view_birthday" => Some("view_birthday"),
            "show_online_status" => Some("show_online_status"),
            "display_seen_status" => Some("display_seen_status"),
            "receive_message" => Some("receive_message"),
            "accept_stranger_call" => Some("accept_stranger_call"),
            "add_friend_via_phone" => Some("add_friend_via_phone"),
            "add_friend_via_qr" => Some("add_friend_via_qr"),
            "add_friend_via_group" => Some("add_friend_via_group"),
            "add_friend_via_contact" => Some("add_friend_via_contact"),
            "display_on_recommend_friend" => Some("display_on_recommend_friend"),
            _ => None,
        };
        if let Some(k) = key_static {
            self.update_privacy_setting(k, code, cx);
        }
    }

    fn update_privacy_setting(&mut self, key: &'static str, code: i64, cx: &mut Context<Self>) {
        if let Some(map) = &mut self.privacy.values {
            map.insert(key.to_owned(), code);
        }
        if cfg!(test) || !self.has_session {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        if !self.privacy.busy.insert(key.to_owned()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.update_setting(key, code)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.privacy.busy.remove(key);
                if let Err(err) = res {
                    view.set_note(format!("Không đổi được cài đặt: {err}"));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_block_list(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) || !self.has_session || self.privacy.blocked_busy {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        self.privacy.blocked_busy = true;
        self.privacy.blocked_err = None;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let mut out = Vec::new();
                        for page in 1..=20 {
                            let val = api
                                .get_block_list(page, 50)
                                .await
                                .map_err(|e| e.to_string())?;
                            let (people, more) = block_list_from(&val);
                            out.extend(people);
                            if !more {
                                break;
                            }
                        }
                        Ok::<_, String>(out)
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.privacy.blocked_busy = false;
                match res {
                    Ok(list) => view.privacy.blocked = Some(list),
                    Err(err) => view.privacy.blocked_err = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn unblock_uid(&mut self, uid: String, cx: &mut Context<Self>) {
        if cfg!(test) || !self.has_session || !self.privacy.unblocking.insert(uid.clone()) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            self.privacy.unblocking.remove(&uid);
            return;
        };
        cx.spawn(async move |this, cx| {
            let u = uid.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.set_block(&u, false)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.privacy.unblocking.remove(&uid);
                match res {
                    Ok(()) => {
                        if let Some(list) = &mut view.privacy.blocked {
                            list.retain(|(id, _)| id != &uid);
                        }
                        view.set_note("Đã bỏ chặn người dùng");
                    }
                    Err(err) => view.set_note(format!("Không bỏ chặn được: {err}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[derive(Clone, Debug)]
struct SRow {
    label: String,
    desc: Option<String>,
    ctl: Ctl,
}

#[derive(Clone, Debug)]
struct Section {
    title: String,
    desc: Option<String>,
    rows: Vec<SRow>,
}

/// Values the Data / Call pages need, refreshed when the page opens.
#[derive(Clone, Debug, Default)]
pub struct SettingsCache {
    media: u64,
    cache: u64,
    system: u64,
    free: String,
    mount: String,
    fs_total: u64,
    fs_used: u64,
    updated: String,
    sources: Vec<(String, String)>,
    sinks: Vec<(String, String)>,
    cameras: Vec<String>,
    tray_hint: String,
}

/// Uniform `(value, label)` options when a select row stores display strings directly.
fn flat_options(values: &[&str]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|v| ((*v).to_owned(), (*v).to_owned()))
        .collect()
}

/// Human-readable label for a [`Ctl::Select`] option, falling back cleanly if an old device ID is unplugged.
fn select_display_label(options: &[(String, String)], current: &str) -> String {
    if let Some((_, label)) = options.iter().find(|(val, _)| val == current) {
        return label.clone();
    }
    if current.is_empty() {
        return "Mặc định".to_owned();
    }
    current.to_owned()
}

/// Local JSON used only by the unit test. Not the PC → phone backup.
fn build_chat_backup_json(
    convs: &[crate::data::ChatConv],
    messages: &HashMap<u64, Vec<crate::data::ChatMsg>>,
    owner: Option<&str>,
) -> String {
    let conv_rows: Vec<serde_json::Value> = convs
        .iter()
        .map(|conv| {
            let msgs: Vec<serde_json::Value> = messages
                .get(&conv.id)
                .into_iter()
                .flatten()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id,
                        "fromMe": m.from_me,
                        "sender": m.sender,
                        "senderUid": m.sender_uid,
                        "text": m.text,
                        "time": m.time,
                        "atMs": m.at_ms,
                        "recalled": m.recalled,
                    })
                })
                .collect();
            serde_json::json!({
                "id": conv.id,
                "threadId": conv.thread_id,
                "name": conv.name,
                "isGroup": conv.is_group,
                "messages": msgs,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "format": "zalo-gpui-backup-v1",
        "owner": owner.unwrap_or(""),
        "exportedAtMs": crate::data::now_ms(),
        "conversationCount": conv_rows.len(),
        "conversations": conv_rows,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_owned())
}

/// Settings switch for other modules (auto-reply rows).
pub(crate) fn switch_el(on: bool) -> impl IntoElement {
    // The glyph takes the text colour: blue when on, neutral disabled grey when off.
    let off = token_rgb("--text-disabled", "#8b96a7");
    div()
        .text_color(rgb(if on {
            token_rgb("--B55", "#0068ff")
        } else {
            off
        }))
        .child(toggle_el(on))
}

/// `fa-toggle-checked-24` / `-unchecked-24` with the white knob.
fn toggle_el(on: bool) -> impl IntoElement {
    div()
        .relative()
        .w(px(38.))
        .h(px(24.))
        .child(
            div()
                .absolute()
                .top(px(4.))
                .left(px(4.))
                .w(px(30.))
                .h(px(16.))
                .rounded(px(10.))
                .bg(rgb(0xffffff)),
        )
        .child(
            // The glyph is 38 × 24 at 24 px: no square clip (unlike `icon_el`).
            div()
                .absolute()
                .inset(px(0.))
                .w(px(38.))
                .h(px(24.))
                .text_size(px(24.))
                .line_height(px(24.))
                .when(fonts::icon_loaded(), |el| {
                    el.font_family(crate::fonts::FAMILY_ICON)
                })
                .child(fonts::glyph(if on { '\u{EDBE}' } else { '\u{EDC0}' }, "◯")),
        )
}

fn sync_dash(blue: u32) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .child(div().w(px(3.)).h(px(3.)).rounded(px(1.5)).bg(rgb(blue)))
        .child(div().w(px(3.)).h(px(3.)).rounded(px(1.5)).bg(rgb(blue)))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(blue))
                .child(fonts::icon_el('\u{EA1B}', "→", 15.)),
        )
        .into_any_element()
}

fn sync_monitor(blue: u32, mark: &'static str) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w(px(46.))
                .h(px(30.))
                .rounded(px(3.))
                .border_1()
                .border_color(rgb(blue))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(16.))
                .text_color(rgb(blue))
                .when(!mark.is_empty(), |el| {
                    el.child(fonts::icon_el('\u{ED8C}', mark, 16.))
                }),
        )
        .child(div().mt(px(2.)).w(px(16.)).h(px(2.)).bg(rgb(blue)))
        .child(div().w(px(24.)).h(px(2.)).bg(rgb(blue)))
        .into_any_element()
}

fn sync_phone(blue: u32) -> gpui::AnyElement {
    div()
        .w(px(22.))
        .h(px(34.))
        .rounded(px(4.))
        .border_1()
        .border_color(rgb(blue))
        .flex()
        .flex_col()
        .items_center()
        .justify_between()
        .py(px(3.))
        .child(div().w(px(8.)).h(px(2.)).rounded(px(1.)).bg(rgb(blue)))
        .child(div().w(px(4.)).h(px(4.)).rounded(px(2.)).bg(rgb(blue)))
        .into_any_element()
}

/// Phone → PC icon for Sync 2.
fn sync_art_phone_to_pc(blue: u32) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .h(px(40.))
        .child(sync_phone(blue))
        .child(sync_dash(blue))
        .child(sync_monitor(blue, ""))
        .into_any_element()
}

/// PC → Phone icon for Message Backup.
fn sync_art_pc_to_phone(blue: u32) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .h(px(40.))
        .child(sync_monitor(blue, ""))
        .child(sync_dash(blue))
        .child(sync_phone(blue))
        .into_any_element()
}

/// Two computers, this client's export/import card.
fn sync_art_pc_to_pc(blue: u32) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .h(px(40.))
        .child(sync_monitor(blue, ""))
        .child(sync_dash(blue))
        .child(sync_monitor(blue, ""))
        .into_any_element()
}

fn dir_size(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

pub(crate) fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / 1024.0 / 1024.0 / 1024.0)
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else if b >= 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// `df` for the folder (or its nearest existing parent): mount point,
/// size, used, available bytes.
fn disk_info(path: &std::path::Path) -> Option<(String, u64, u64, u64)> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
    }
    let out = std::process::Command::new("df")
        .args(["-B1", "--output=target,size,used,avail"])
        .arg(&probe)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 4 {
        return None;
    }
    Some((
        f[0].to_owned(),
        f[1].parse().ok()?,
        f[2].parse().ok()?,
        f[3].parse().ok()?,
    ))
}

/// Move `photos/` and `thumbs/` from `from` to `to` (rename, or copy + delete
/// across filesystems). Returns files moved.
fn move_media(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<usize> {
    let mut n = 0;
    for sub in ["photos", "thumbs"] {
        let src = from.join(sub);
        let dst = to.join(sub);
        std::fs::create_dir_all(&dst)?;
        let Ok(entries) = std::fs::read_dir(&src) else {
            continue;
        };
        for e in entries.flatten() {
            let target = dst.join(e.file_name());
            if std::fs::rename(e.path(), &target).is_err() {
                std::fs::copy(e.path(), &target)?;
                std::fs::remove_file(e.path())?;
            }
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_matches_css_and_catalogue() {
        assert_eq!(SETTING_MENU_W, 228.0);
        assert_eq!(SETTING_MODAL_W, 800.0);
        assert_eq!(fmt_size(212_300_000), "202.5 MB");
        assert_eq!(SettingsPage::ALL.len(), 11);
        assert_eq!(SettingsPage::General.title(), "Cài đặt chung");
        assert_eq!(SettingsPage::Notify.title(), "Thông báo");
        assert_eq!(SettingsPage::Utilities.title(), "Tiện ích");
        assert_eq!(SettingsPage::Advanced.title(), "Nâng cao");
        assert_eq!(*SettingsPage::ALL.last().unwrap(), SettingsPage::Advanced);
        assert_eq!(str_vi("STR_ZALO_SETTING", ""), "Cài đặt");
    }

    #[test]
    fn privacy_and_block_list_parsers_and_codes() {
        let raw = serde_json::json!({
            "data": {
                "show_online_status": true,
                "display_seen_status": 0,
                "receive_message": "2",
                "accept_stranger_call": 4,
                "view_birthday": 2
            }
        });
        let map = privacy_from(&raw);
        assert_eq!(map.get("show_online_status"), Some(&1));
        assert_eq!(map.get("display_seen_status"), Some(&0));
        assert_eq!(map.get("receive_message"), Some(&2));
        assert_eq!(map.get("accept_stranger_call"), Some(&4));
        assert_eq!(map.get("view_birthday"), Some(&2));

        let lbl = option_label(&CALL_OPTS, 4);
        assert_eq!(lbl, "Bạn bè và người lạ từng liên hệ");
        assert_eq!(option_code(&CALL_OPTS, &lbl), 4);
        assert_eq!(option_code(&MSG_OPTS, &option_label(&MSG_OPTS, 2)), 2);
        assert_eq!(
            option_code(&BIRTHDAY_OPTS, &option_label(&BIRTHDAY_OPTS, 2)),
            2
        );

        let blk = serde_json::json!({
            "hasMore": 1,
            "profiles": [{"userId": "42", "displayName": "An"}, {"zaloName": "no id"}]
        });
        let (people, more) = block_list_from(&blk);
        assert_eq!(people, vec![("42".to_owned(), "An".to_owned())]);
        assert!(more);
    }

    #[test]
    fn select_display_label_and_backup_json_work_offline() {
        let opts = vec![
            (String::new(), "Mặc định".to_owned()),
            ("alsa_input.usb".to_owned(), "Micro USB Podcast".to_owned()),
        ];
        assert_eq!(select_display_label(&opts, ""), "Mặc định");
        assert_eq!(
            select_display_label(&opts, "alsa_input.usb"),
            "Micro USB Podcast"
        );
        assert_eq!(
            select_display_label(&opts, "alsa_input.old"),
            "alsa_input.old"
        );
        assert_eq!(
            flat_options(&["Tiếng Việt", "English"]),
            vec![
                ("Tiếng Việt".to_owned(), "Tiếng Việt".to_owned()),
                ("English".to_owned(), "English".to_owned()),
            ]
        );

        let conv = crate::data::ChatConv {
            id: 7,
            thread_id: "uid-7".into(),
            name: "Minh".into(),
            preview: "Chào bạn".into(),
            time: "10:00".into(),
            unread: 0,
            is_group: false,
            online: true,
            live: false,
        };
        let mut msgs = HashMap::new();
        msgs.insert(
            7,
            vec![crate::data::ChatMsg {
                id: 11,
                conv: 7,
                thread_id: "uid-7".into(),
                from_me: true,
                sender: "Tôi".into(),
                text: "Chào bạn".into(),
                time: "10:00".into(),
                at_ms: 1_700_000_000_000,
                ..Default::default()
            }],
        );
        let raw = build_chat_backup_json(&[conv], &msgs, Some("Lan"));
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["format"], "zalo-gpui-backup-v1");
        assert_eq!(parsed["owner"], "Lan");
        assert_eq!(parsed["conversationCount"], 1);
        assert_eq!(
            parsed["conversations"][0]["messages"][0]["text"],
            "Chào bạn"
        );

        assert_eq!(tfa_enabled_from(None), None);
        let mut m = HashMap::new();
        assert_eq!(tfa_enabled_from(Some(&m)), Some(false));
        m.insert("setting_2FA_status".into(), 1);
        assert_eq!(tfa_enabled_from(Some(&m)), Some(true));
    }
}
