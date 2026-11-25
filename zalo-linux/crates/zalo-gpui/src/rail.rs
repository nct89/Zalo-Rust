//! `#main-tab` 64 px, full window height through the titlebar. `--B60` `#005ae0`.

use gpui::{
    div, hsla, prelude::*, px, rgb, rgba, ClickEvent, Context, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled,
};

use crate::color::{str_vi, token_rgb};
use crate::context_menu::MenuKind;
use crate::fonts::{self, ic, FAMILY_SEMIBOLD};
use crate::shell::ShellView;
use crate::widgets::tip;
use crate::window_frame::CsdCorners;

/// Far-left icon rail. Matches egui `Rail` order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rail {
    /// Messages — conversation list.
    #[default]
    Chats,
    /// Contacts — four contact entries.
    Contacts,
    /// Settings — local placeholder page.
    Settings,
    /// Tools — local placeholder page.
    Tools,
    /// zCloud — local placeholder, no live API.
    Cloud,
    /// My Cloud — selects the preview Cloud thread.
    Docs,
}

/// Popover anchored to the rail (live DOM `.popover-v3` at x = 64).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailPopup {
    /// `.zcloud-promo-content` 336 × 374.
    ZCloud,
    /// "zBusiness Tools" 314 × 291, 3-column tiles.
    Tools,
    /// Product catalogs from `/api/prodcatalog/catalog/list`.
    Catalog,
}

/// Selected chip: black α 64. Hover: black α 38. Radius 6.
const MARK_ON: u32 = 0x0000_0040;
const MARK_HOVER: u32 = 0x0000_0026;

/// `.mmi-icon-wr` box: `#main-tab` 64 − 16 = 48.
pub const RAIL_MARK_W: f32 = zalo_design::MAIN_TAB_W - 16.0;

// Live DOM of Zalo PC 26.10.10 (CDP, 11/11/2026), not the unused CSS rules
// behind `NAV_BOTTOM_BTN_H` 60 / `NAV_BOTTOM_ICON_PX` 24:
//   #main-tab padding-top 14 → .nav__tabs__zalo 92 (padding-top 32, avatar 40)
//   every .leftbar-tab 56 tall, .mmi-icon-wr 48×48 at +4, icon 28 px
//   (zCloud 30 px), .nav__tabs__bottom padding-bottom 12.
/// `#main-tab` padding-top.
pub const RAIL_PAD_TOP: f32 = 14.0;
/// `.zavatar-m` in `.nav__tabs__zalo`.
pub const RAIL_AVATAR: f32 = 40.0;
/// Every `.leftbar-tab`, top and bottom.
pub const RAIL_TAB_H: f32 = 56.0;
/// `.internal-icon` font-size on every tab.
pub const RAIL_ICON_PX: f32 = 28.0;
/// `fa-zCloud_24_Line.sidebar-icon` is drawn at 30 px.
pub const RAIL_CLOUD_ICON_PX: f32 = 30.0;
/// `.nav__tabs__bottom` padding-bottom.
pub const RAIL_PAD_BOTTOM: f32 = 12.0;
/// `.z-noti-badge --big`: 16 px circle, `#c31818`, at (+26, +6) of the mark.
pub const RAIL_BADGE: f32 = 16.0;

/// Conversation list stays visible for every rail except Contacts.
pub fn lists_chats(rail: Rail) -> bool {
    !matches!(rail, Rail::Contacts)
}

impl ShellView {
    /// Full-height `#main-tab`.
    pub(crate) fn rail_column(
        &self,
        corners: CsdCorners,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rail_bg = token_rgb("--layer-background-navbar-normal", "#005ae0");
        let rail_ink = token_rgb("--icon-navbar-normal", "#ffffff");
        let selected = self.rail;
        let gear_on = self.settings_open
            || matches!(self.menu.as_ref().map(|m| &m.kind), Some(MenuKind::Gear));
        corners
            .apply(
                div()
                    .id("main-tab")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w(px(zalo_design::MAIN_TAB_W))
                    .h_full()
                    .bg(rgb(rail_bg)),
                true,
                false,
                true,
                false,
            )
            .text_color(rgb(rail_ink))
            .pt(px(RAIL_PAD_TOP))
            .child(self.rail_logo(cx))
            .child(self.rail_top_tab(
                "rail-chats",
                if selected == Rail::Chats {
                    ic::CHAT_ON
                } else {
                    ic::CHAT
                },
                "▣",
                str_vi("STR_TAB_MESSAGE", "Tin nhắn"),
                selected == Rail::Chats,
                Rail::Chats,
                self.total_unread(),
                cx,
            ))
            .child(self.rail_top_tab(
                "rail-contacts",
                if selected == Rail::Contacts {
                    ic::CONTACTS_ON
                } else {
                    ic::CONTACTS
                },
                "☺",
                str_vi("STR_TAB_CONTACT", "Danh bạ"),
                selected == Rail::Contacts,
                Rail::Contacts,
                0,
                cx,
            ))
            .child(div().flex_1())
            .child(self.rail_bottom_tab(
                "rail-zcloud",
                if self.rail_popup == Some(RailPopup::ZCloud) {
                    ic::CLOUD_ON
                } else {
                    ic::CLOUD
                },
                "☁",
                str_vi("STR_LEFT_SIDEBAR_ZALO_CLOUD", "Zalo Cloud"),
                self.rail_popup == Some(RailPopup::ZCloud),
                Some(Rail::Cloud),
                cx,
            ))
            .child(self.rail_bottom_tab(
                "rail-docs",
                if selected == Rail::Docs {
                    ic::DOCS_ON
                } else {
                    ic::DOCS
                },
                "☁",
                "My Documents",
                selected == Rail::Docs,
                Some(Rail::Docs),
                cx,
            ))
            .child(self.rail_bottom_tab(
                "rail-shot",
                ic::CAPTURE,
                "▢",
                str_vi("STR_SCREENSHOT", "Chụp màn hình"),
                false,
                None,
                cx,
            ))
            .child(self.rail_bottom_tab(
                "rail-tools",
                if self.starred_open
                    || matches!(self.rail_popup, Some(RailPopup::Tools | RailPopup::Catalog))
                {
                    ic::BRIEFCASE_ON
                } else {
                    ic::BRIEFCASE
                },
                "☰",
                str_vi("STR_TOOLS", "Công cụ"),
                self.starred_open
                    || matches!(self.rail_popup, Some(RailPopup::Tools | RailPopup::Catalog)),
                Some(Rail::Tools),
                cx,
            ))
            .child(self.rail_bottom_tab(
                "rail-settings",
                if gear_on { ic::GEAR_ON } else { ic::GEAR },
                "⚙",
                str_vi("STR_TAB_SETTING", "Cài đặt"),
                gear_on,
                Some(Rail::Settings),
                cx,
            ))
            .child(div().flex_none().h(px(RAIL_PAD_BOTTOM)))
    }

    /// Sum of unread badges for the Messages tab.
    pub(crate) fn total_unread(&self) -> u32 {
        self.convs
            .iter()
            .filter(|row| !self.hidden.contains(&row.id) && !self.removed.contains(&row.id))
            .map(|row| self.unread_of(row))
            .sum()
    }

    fn rail_logo(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tip_text = str_vi("STR_ACCOUNT_MENU", "Hồ sơ của bạn");
        let glyph = px(RAIL_AVATAR);
        div()
            .id("nav-tabs-zalo")
            .flex()
            .flex_none()
            .justify_center()
            .items_start()
            .w_full()
            .h(px(zalo_design::NAV_LOGO_H))
            .pt(px(zalo_design::NAV_LOGO_PAD_TOP))
            .cursor_pointer()
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                this.open_menu(MenuKind::Account, event.position(), window);
                cx.notify();
            }))
            .tooltip(move |_, cx| tip(tip_text, cx))
            .when_some(self.avatar_of(crate::profiles::ME), |el, p| {
                el.child(
                    gpui::img(p)
                        .size(glyph)
                        .rounded(px(RAIL_AVATAR * 0.5))
                        .object_fit(gpui::ObjectFit::Cover),
                )
            })
            .when(self.avatar_of(crate::profiles::ME).is_none(), |el| {
                el.child(
                    div()
                        .size(glyph)
                        .rounded(px(RAIL_AVATAR * 0.5))
                        .border_1()
                        .border_color(rgb(0xffffff))
                        .bg(hsla(0., 0., 1., 0.18))
                        .flex()
                        .items_center()
                        .justify_center()
                        .overflow_hidden()
                        .text_size(px(20.))
                        .font_family(FAMILY_SEMIBOLD)
                        .child("Z"),
                )
            })
    }

    fn rail_top_tab(
        &self,
        id: &'static str,
        code: char,
        fallback: &'static str,
        label: &'static str,
        on: bool,
        rail: Rail,
        badge: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        mark_btn(id, code, fallback, label, on, RAIL_ICON_PX, badge).on_click(cx.listener(
            move |this, _, _, cx| {
                this.clear_sidebar_search(cx);
                this.set_rail(rail);
                cx.notify();
            },
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn rail_bottom_tab(
        &self,
        id: &'static str,
        code: char,
        fallback: &'static str,
        label: &'static str,
        on: bool,
        rail: Option<Rail>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let screenshot = rail.is_none();
        let icon_px = if matches!(rail, Some(Rail::Cloud)) {
            RAIL_CLOUD_ICON_PX
        } else {
            RAIL_ICON_PX
        };
        mark_btn(id, code, fallback, label, on, icon_px, 0).on_click(cx.listener(
            move |this, event: &ClickEvent, window, cx| {
                if screenshot {
                    this.capture_screenshot(Some(window.window_handle()), cx);
                } else if matches!(rail, Some(Rail::Settings)) {
                    this.clear_sidebar_search(cx);
                    this.rail_popup = None;
                    if matches!(this.menu.as_ref().map(|m| &m.kind), Some(MenuKind::Gear)) {
                        this.menu = None;
                    } else {
                        this.open_menu(MenuKind::Gear, event.position(), window);
                    }
                } else if let Some(rail) = rail {
                    this.clear_sidebar_search(cx);
                    let popup = match rail {
                        Rail::Cloud => Some(RailPopup::ZCloud),
                        Rail::Tools => Some(RailPopup::Tools),
                        _ => None,
                    };
                    match popup {
                        Some(p) => {
                            this.menu = None;
                            this.rail_popup = if this.rail_popup == Some(p) {
                                None
                            } else {
                                Some(p)
                            };
                        }
                        None => {
                            this.rail_popup = None;
                            this.set_rail(rail);
                        }
                    }
                }
                cx.notify();
            },
        ))
    }
}

impl ShellView {
    /// Dismiss layer + the open rail popover (occludes clicks underneath).
    pub(crate) fn rail_popup_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(popup) = self.rail_popup else {
            return div().into_any_element();
        };
        let panel = match popup {
            RailPopup::ZCloud => self.zcloud_panel(cx).into_any_element(),
            RailPopup::Tools => self.tools_panel(cx).into_any_element(),
            RailPopup::Catalog => self.catalog_panel(cx).into_any_element(),
        };
        let popup_box = div()
            .absolute()
            .left(px(zalo_design::MAIN_TAB_W))
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(panel);
        let positioned = match popup {
            RailPopup::ZCloud => popup_box.bottom(px(77.)),
            RailPopup::Tools | RailPopup::Catalog => popup_box.bottom(px(68.)),
        };
        div()
            .id("rail-popup-layer")
            .absolute()
            .inset(px(0.))
            .child(
                div()
                    .id("rail-popup-dismiss")
                    .absolute()
                    .inset(px(0.))
                    .occlude()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.rail_popup = None;
                        cx.notify();
                    })),
            )
            .child(positioned)
            .into_any_element()
    }

    fn zcloud_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // `.zcloud-promo-content`: 336 wide, image 196 on `#e5f8ff`, theme
        // surface body (title 16/500, body 14/18), primary blue CTA 304 × 40.
        let image = crate::media::png_asset("zcloud-promo.png", 672);
        let paper = token_rgb("--layer-background", "#ffffff");
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let cta_bg = token_rgb("--button-primary-normal", "#0068ff");
        let cta_hover = token_rgb("--button-primary-hover", "#005ae0");
        div()
            .w(px(336.))
            .rounded(px(8.))
            .overflow_hidden()
            .bg(rgb(paper))
            .border_1()
            .border_color(crate::color::border_subtle())
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .h(px(196.))
                    .bg(rgb(if crate::color::is_dark() {
                        0x1c_1c_1c
                    } else {
                        0xe5_f8_ff
                    }))
                    .when_some(image, |el, img| {
                        el.child(gpui::img(img).w(px(336.)).h(px(196.)).object_fit(gpui::ObjectFit::Cover))
                    })
                    .child(
                        div()
                            .id("zcloud-close")
                            .absolute()
                            .top(px(6.))
                            .right(px(6.))
                            .size(px(28.))
                            .rounded(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .bg(if crate::color::is_dark() {
                                hsla(0., 0., 0., 0.55)
                            } else {
                                hsla(0., 0., 1., 0.65)
                            })
                            .text_color(rgb(token_rgb("--text-primary", "#081b3a")))
                            .hover(|st| {
                                st.bg(if crate::color::is_dark() {
                                    hsla(0., 0., 0., 0.8)
                                } else {
                                    hsla(0., 0., 1., 0.9)
                                })
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.rail_popup = None;
                                cx.notify();
                            }))
                            .child(fonts::icon_el('\u{EA9F}', "×", 18.)),
                    ),
            )
            .child(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .line_height(px(20.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child("Bảo toàn dữ liệu Zalo với zCloud"),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .line_height(px(18.))
                            .text_color(rgb(muted))
                            .child("Khám phá ngay tiện ích lưu trữ an toàn ảnh, video, và tệp quan trọng trên zCloud, đồng thời giải phóng dung lượng máy."),
                    ),
            )
            .child(
                div().px(px(16.)).pb(px(16.)).child(
                    div()
                        .id("zcloud-cta")
                        .h(px(40.))
                        .rounded(px(4.))
                        .bg(rgb(cta_bg))
                        .hover(move |st| st.bg(rgb(cta_hover)))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(15.))
                        .font_family(FAMILY_SEMIBOLD)
                        .text_color(rgb(crate::color::on_accent_fg()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            // Zalo PC opens this paywall in an embedded web window.
                            cx.open_url("https://cloud.zbox.vn/pc-paywall?utm_source=pc_entry&utm_medium=leftsidebar");
                            this.rail_popup = None;
                            cx.notify();
                        }))
                        .child("Tìm hiểu zCloud ngay"),
                ),
            )
    }

    fn tools_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // `.more-pop-title` 14/500 padding 16 16 4; `.more-pop-item` 88 × 110,
        // margin 4, padding 16 8 0, radius 4, icon 32 `#005ae0`, label 14/21.
        let ink = token_rgb("--text-primary", "#081b3a");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let tiles: [(char, &'static str, Option<ToolTile>); 5] = [
            ('\u{ECEB}', "Tin nhắn nhanh", Some(ToolTile::Quick)),
            ('\u{EA52}', "Danh mục sản phẩm", Some(ToolTile::Catalog)),
            ('\u{EA24}', "Trả lời\ntự động", Some(ToolTile::AutoReply)),
            ('\u{ED73}', "Tin\nđánh dấu", Some(ToolTile::Starred)),
            ('\u{EA3A}', "Tin\nđồng thời", Some(ToolTile::Broadcast)),
        ];
        let info_blue = token_rgb("--text-information", "#005ae0");
        let disabled_c = token_rgb("--text-disabled", "#8b96a7");
        div()
            .w(px(314.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .border_1()
            .border_color(crate::color::border_subtle())
            .shadow_lg()
            .pb(px(12.))
            .child(
                div()
                    .pt(px(16.))
                    .px(px(16.))
                    .pb(px(4.))
                    .text_size(px(14.))
                    .line_height(px(21.))
                    .font_family(FAMILY_SEMIBOLD)
                    .text_color(rgb(ink))
                    .child("Công cụ zBusiness"),
            )
            .child({
                // Inner width is exactly 3 × 96 = 288: fixed rows of 3, no wrap rounding.
                let mut cells: Vec<gpui::AnyElement> = Vec::new();
                for (i, (code, label, action)) in tiles.into_iter().enumerate() {
                    cells.push({
                        let enabled = action.is_some();
                        div()
                            .id(("tool-tile", i as u64))
                            .w(px(88.))
                            .h(px(110.))
                            .m(px(4.))
                            .pt(px(16.))
                            .px(px(8.))
                            .rounded(px(4.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .when(enabled, |el| {
                                el.cursor_pointer().hover(|st| st.bg(rgb(hover)))
                            })
                            .when_some(action, |el, action| {
                                el.on_click(cx.listener(move |this, _, _, cx| {
                                    match action {
                                        ToolTile::Starred => {
                                            this.rail_popup = None;
                                            this.starred_open = true;
                                            this.compact_show_chat = false;
                                            if this.rail == Rail::Contacts {
                                                this.rail = Rail::Chats;
                                            }
                                        }
                                        ToolTile::Catalog => {
                                            this.rail_popup = Some(RailPopup::Catalog)
                                        }
                                        ToolTile::Quick => this.open_quick_messages(cx),
                                        ToolTile::AutoReply => {
                                            this.rail_popup = None;
                                            this.open_auto_reply(cx);
                                        }
                                        ToolTile::Broadcast => {
                                            this.open_broadcast(cx);
                                        }
                                    }
                                    cx.notify();
                                }))
                            })
                            .when(!enabled, |el| {
                                el.tooltip(|_, cx| tip("Bản Linux chưa hỗ trợ mục này", cx))
                            })
                            .child(
                                div()
                                    .mb(px(8.))
                                    .text_color(rgb(if enabled { info_blue } else { disabled_c }))
                                    .child(fonts::icon_el(code, "•", 32.)),
                            )
                            .child(
                                div()
                                    .w(px(72.))
                                    .text_size(px(14.))
                                    .line_height(px(21.))
                                    .text_center()
                                    .whitespace_normal()
                                    .text_color(rgb(if enabled { ink } else { disabled_c }))
                                    .child(crate::i18n::t(&label)),
                            )
                            .into_any_element()
                    });
                }
                let mut grid = div().flex().flex_col().px(px(12.));
                let mut it = cells.into_iter().peekable();
                while it.peek().is_some() {
                    let row: Vec<gpui::AnyElement> = it.by_ref().take(3).collect();
                    grid = grid.child(div().flex().flex_row().children(row));
                }
                grid
            })
    }

    /// Left-sidebar "Tin đánh dấu" view matching Zalo PC (replaces conversation list under search bar).
    pub(crate) fn starred_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let subtle = token_rgb("--layer-background-subtle", "#ebecf0");
        let star_gold = token_rgb("--Y60", "#e0ac00");
        let line = crate::color::border_vanish();
        let query = self.search_query.trim().to_lowercase();

        let mut all_items: Vec<(i64, u64, u64, String, String, String, String)> = Vec::new();
        for conv in &self.convs {
            for msg in self.messages_of(conv.id) {
                if self.starred.contains(&msg.id) {
                    let preview = starred_msg_preview(msg);
                    let sender = if conv.is_group && !msg.from_me && !msg.sender.is_empty() {
                        msg.sender.clone()
                    } else if msg.from_me {
                        str_vi("STR_YOU", "Bạn").to_owned()
                    } else {
                        String::new()
                    };
                    all_items.push((
                        msg.at_ms,
                        msg.id,
                        conv.id,
                        conv.name.clone(),
                        sender,
                        preview,
                        msg.time.clone(),
                    ));
                }
            }
        }
        all_items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        let total = all_items.len();
        let items: Vec<_> = if query.is_empty() {
            all_items
        } else {
            all_items
                .into_iter()
                .filter(|(_, _, _, conv_name, sender, preview, _)| {
                    conv_name.to_lowercase().contains(&query)
                        || sender.to_lowercase().contains(&query)
                        || preview.to_lowercase().contains(&query)
                })
                .collect()
        };

        let title = str_vi("STR_STARRED_MESSAGES_0", "Tin đánh dấu");
        let empty_copy = str_vi(
            "STR_STAR_EMPTY_LIST",
            "Đánh dấu tin nhắn để có thể tìm lại một cách nhanh chóng. Các tin nhắn được đánh dấu sẽ xuất hiện ở đây.",
        );
        let unstar_tip = str_vi("STR_TOOLTIP_UNSTAR_MSG", "Bỏ đánh dấu tin nhắn");
        let close_tip = str_vi("STR_CLOSE", "Đóng");

        let star_badge_bg = if crate::color::is_dark() {
            hsla(42. / 360., 0.65, 0.50, 0.14)
        } else {
            hsla(42. / 360., 0.92, 0.55, 0.16)
        };
        let star_ring = if crate::color::is_dark() {
            hsla(42. / 360., 0.65, 0.55, 0.28)
        } else {
            hsla(42. / 360., 0.85, 0.50, 0.30)
        };

        div()
            .id("starred-sidebar")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(px(16.))
                    .pt(px(4.))
                    .pb(px(12.))
                    .border_b_1()
                    .border_color(line)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .line_height(px(22.))
                                    .font_family(FAMILY_SEMIBOLD)
                                    .text_color(rgb(ink))
                                    .child(title),
                            )
                            .when(total > 0, |el| {
                                el.child(
                                    div()
                                        .px(px(7.))
                                        .py(px(1.))
                                        .rounded(px(10.))
                                        .bg(rgb(subtle))
                                        .text_size(px(12.))
                                        .text_color(rgb(muted))
                                        .child(total.to_string()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("starred-sidebar-close")
                            .size(px(28.))
                            .rounded(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(rgb(muted))
                            .hover(move |st| st.bg(rgb(hover)).text_color(rgb(ink)))
                            .tooltip(move |_, cx| tip(close_tip, cx))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.starred_open = false;
                                this.clear_sidebar_search(cx);
                                cx.notify();
                            }))
                            .child(fonts::icon_el('\u{EA9F}', "×", 16.)),
                    ),
            )
            .when(total == 0, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_1()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .px(px(32.))
                        .pb(px(56.))
                        .gap(px(18.))
                        .child(
                            div()
                                .size(px(84.))
                                .rounded(px(42.))
                                .bg(star_badge_bg)
                                .border_1()
                                .border_color(star_ring)
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(star_gold))
                                .child(fonts::icon_el('\u{ED73}', "★", 40.)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(2.))
                                .text_size(px(13.5))
                                .line_height(px(20.))
                                .text_color(rgb(muted))
                                .children(
                                    crate::media::wrap_ui_note(empty_copy, 260.)
                                        .lines()
                                        .map(|line| {
                                            div()
                                                .whitespace_nowrap()
                                                .text_center()
                                                .child(line.to_owned())
                                        })
                                        .collect::<Vec<_>>(),
                                ),
                        ),
                )
            })
            .when(total > 0 && items.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .px(px(24.))
                        .text_size(px(13.))
                        .text_center()
                        .text_color(rgb(muted))
                        .child(crate::i18n::t("Không tìm thấy tin nhắn đánh dấu phù hợp.")),
                )
            })
            .when(!items.is_empty(), |el| {
                el.child(
                    div()
                        .id("starred-list")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .py(px(4.))
                        .children(items.into_iter().enumerate().map(
                            |(i, (_, msg_id, conv_id, conv_name, sender, preview, time))| {
                                let snippet = if sender.is_empty() {
                                    preview
                                } else {
                                    format!("{sender}: {preview}")
                                };
                                div()
                                    .id(("starred", i as u64))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .px(px(16.))
                                    .py(px(10.))
                                    .border_b_1()
                                    .border_color(line)
                                    .cursor_pointer()
                                    .hover(move |st| st.bg(rgb(hover)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.jump_to_msg_id(conv_id, msg_id);
                                        cx.notify();
                                    }))
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
                                                    .truncate()
                                                    .text_size(px(13.))
                                                    .font_family(FAMILY_SEMIBOLD)
                                                    .text_color(rgb(ink))
                                                    .child(conv_name),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_none()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap(px(6.))
                                                    .when(!time.is_empty(), |el| {
                                                        el.child(
                                                            div()
                                                                .text_size(px(11.))
                                                                .text_color(rgb(muted))
                                                                .child(time),
                                                        )
                                                    })
                                                    .child(
                                                        div()
                                                            .id(("starred-rm", i as u64))
                                                            .size(px(22.))
                                                            .rounded(px(11.))
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .cursor_pointer()
                                                            .text_color(rgb(star_gold))
                                                            .hover(move |st| st.bg(rgb(subtle)))
                                                            .tooltip(move |_, cx| {
                                                                tip(unstar_tip, cx)
                                                            })
                                                            .on_mouse_down(
                                                                gpui::MouseButton::Left,
                                                                |_, _, cx| cx.stop_propagation(),
                                                            )
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    cx.stop_propagation();
                                                                    this.starred.remove(&msg_id);
                                                                    this.save_starred();
                                                                    cx.notify();
                                                                },
                                                            ))
                                                            .child(fonts::icon_el(
                                                                '\u{ED73}', "★", 13.,
                                                            )),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .line_height(px(18.))
                                            .text_color(rgb(muted))
                                            .truncate()
                                            .child(snippet),
                                    )
                            },
                        )),
                )
            })
    }
}

fn starred_msg_preview(msg: &crate::data::ChatMsg) -> String {
    let trimmed = msg.text.trim();
    if !trimmed.is_empty() {
        return trimmed.replace('\n', " ");
    }
    match msg.kind {
        crate::media::MsgKind::Photo => format!("[{}]", str_vi("STR_PHOTO", "Hình ảnh")),
        crate::media::MsgKind::Video => format!("[{}]", str_vi("STR_VIDEO", "Video")),
        crate::media::MsgKind::Sticker => format!("[{}]", str_vi("STR_STICKER", "Sticker")),
        crate::media::MsgKind::Gif => "[GIF]".to_owned(),
        crate::media::MsgKind::Voice => format!("[{}]", str_vi("STR_VOICE_MESSAGE", "Tin nhắn thoại")),
        crate::media::MsgKind::File => {
            if msg.file_name.is_empty() {
                format!("[{}]", str_vi("STR_FILE", "Tệp"))
            } else {
                format!("[{}] {}", str_vi("STR_FILE", "Tệp"), msg.file_name)
            }
        }
        crate::media::MsgKind::Link => {
            if !msg.href.is_empty() {
                msg.href.clone()
            } else {
                format!("[{}]", str_vi("STR_LINK", "Liên kết"))
            }
        }
        crate::media::MsgKind::Card => format!("[{}]", str_vi("STR_CONTACT_CARD", "Danh thiếp")),
        crate::media::MsgKind::Poll => format!("[{}]", str_vi("STR_POLL", "Bình chọn")),
        crate::media::MsgKind::Location => format!("[{}]", str_vi("STR_LOCATION", "Vị trí")),
        crate::media::MsgKind::Doodle => "[Hình vẽ]".to_owned(),
        crate::media::MsgKind::Ecard => "[Thiệp]".to_owned(),
        crate::media::MsgKind::Call | crate::media::MsgKind::MissedCall => {
            format!("[{}]", str_vi("STR_CALL", "Cuộc gọi"))
        }
        crate::media::MsgKind::Text => String::new(),
    }
}

#[derive(Clone, Copy)]
enum ToolTile {
    Starred,
    Catalog,
    Quick,
    AutoReply,
    Broadcast,
}

impl ShellView {
    /// Names from `/api/prodcatalog/catalog/list`.
    fn catalog_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = token_rgb("--text-primary", "#081b3a");
        let muted = token_rgb("--text-secondary", "#5a6981");
        let hover = token_rgb("--layer-background-hover", "#f1f2f4");
        let empty = self.catalog_rows.is_empty();
        let busy = self.catalog_busy;
        div()
            .w(px(314.))
            .max_h(px(420.))
            .rounded(px(8.))
            .bg(rgb(token_rgb("--layer-background", "#ffffff")))
            .border_1()
            .border_color(crate::color::border_subtle())
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .pt(px(14.))
                    .px(px(16.))
                    .pb(px(8.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child("Danh mục sản phẩm"),
                    )
                    .child(
                        div()
                            .id("catalog-refresh")
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(hover)))
                            .text_size(px(12.))
                            .text_color(rgb(crate::color::link_fg()))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.reload_catalog(cx);
                                cx.notify();
                            }))
                            .child(if busy { "Đang tải…" } else { "Tải lại" }),
                    ),
            )
            .child({
                let note = if self.catalog_note.is_empty() {
                    "Danh mục sản phẩm trên tài khoản Zalo Business"
                } else {
                    self.catalog_note.as_str()
                };
                div()
                    .px(px(16.))
                    .pb(px(10.))
                    .text_size(px(12.))
                    .text_color(rgb(muted))
                    .child(crate::media::wrap_ui_note(note, 276.))
            })
            .when(empty && self.catalog_note.is_empty() && !busy, |el| {
                el.child(
                    div()
                        .px(px(16.))
                        .pb(px(16.))
                        .text_size(px(13.))
                        .text_color(rgb(muted))
                        .child("Chưa có danh mục sản phẩm nào."),
                )
            })
            .child(
                div()
                    .id("catalog-list")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .pb(px(8.))
                    .children(self.catalog_rows.iter().enumerate().map(|(i, (id, name))| {
                        let _ = id;
                        div()
                            .id(("catalog", i as u64))
                            .px(px(16.))
                            .py(px(8.))
                            .hover(move |s| s.bg(rgb(hover)))
                            .text_size(px(14.))
                            .text_color(rgb(ink))
                            .child(name.clone())
                    })),
            )
    }
}

fn mark_btn(
    id: &'static str,
    code: char,
    fallback: &'static str,
    label: &'static str,
    on: bool,
    icon_px: f32,
    badge: u32,
) -> gpui::Stateful<gpui::Div> {
    let inset = (zalo_design::MAIN_TAB_W - RAIL_MARK_W) / 2.0;
    let mark = px(RAIL_MARK_W);
    let radius = px(zalo_design::LEFTBAR_MARK_RADIUS);
    let mark_on = if crate::color::is_dark() {
        0xffff_ff1e
    } else {
        MARK_ON
    };
    let mark_hover = if crate::color::is_dark() {
        0xffff_ff10
    } else {
        MARK_HOVER
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .flex_row()
        .items_center()
        .w_full()
        .h(px(RAIL_TAB_H))
        .px(px(inset))
        .cursor_pointer()
        .tooltip(move |_, cx| tip(label, cx))
        .child(
            div()
                .relative()
                .size(mark)
                .rounded(radius)
                .flex()
                .items_center()
                .justify_center()
                .when(on, |el| el.bg(rgba(mark_on)))
                .when(!on, |el| el.hover(|style| style.bg(rgba(mark_hover))))
                .child(fonts::icon_el(code, fallback, icon_px))
                .when(badge > 0, |el| el.child(rail_badge(badge))),
        )
}

/// Red unread counter pinned to the mark's top-right (`.z-noti-badge --big`).
fn rail_badge(count: u32) -> impl IntoElement {
    let text = if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    };
    let wide = text.chars().count() > 1;
    div()
        .absolute()
        .top(px(6.))
        .left(px(26.))
        .h(px(RAIL_BADGE))
        .min_w(px(RAIL_BADGE))
        .when(wide, |el| el.px(px(4.)))
        .rounded(px(RAIL_BADGE / 2.0))
        .bg(rgb(token_rgb("--R60", "#c31818")))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .line_height(px(RAIL_BADGE))
        .font_family(FAMILY_SEMIBOLD)
        .text_color(rgb(0xffffff))
        .child(crate::i18n::t(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_mark_is_48() {
        assert_eq!(RAIL_MARK_W, 48.0);
        assert_eq!(zalo_design::MAIN_TAB_W, 64.0);
        assert_eq!(zalo_design::NAV_LOGO_H, 92.0);
        // Live DOM: 14 + 92 + 2×56 + … + 5×56 + 12, avatar 40, icons 28.
        assert_eq!(RAIL_PAD_TOP + zalo_design::NAV_LOGO_H, 106.0);
        assert_eq!(RAIL_TAB_H - RAIL_MARK_W, 8.0);
        assert_eq!(RAIL_AVATAR, 40.0);
        assert_eq!(RAIL_ICON_PX, 28.0);
        assert_eq!(zalo_design::NAV_LOGO_GLYPH, 43.0);
        assert_eq!(zalo_design::LEFTBAR_TAB_H, 56.0);
        assert_eq!(zalo_design::NAV_BOTTOM_BTN_H, 60.0);
        assert_eq!(zalo_design::LEFTBAR_ICON_PX, 28.0);
        assert_eq!(zalo_design::NAV_BOTTOM_ICON_PX, 24.0);
        assert_eq!((zalo_design::MAIN_TAB_W - RAIL_MARK_W) / 2.0, 8.0);
        assert_eq!(zalo_design::LEFTBAR_MARK_RADIUS, 6.0);
        assert!(lists_chats(Rail::Chats));
        assert!(!lists_chats(Rail::Contacts));
    }
}
