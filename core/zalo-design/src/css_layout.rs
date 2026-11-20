//! Layout numbers extracted from decoded Zalo PC CSS / JS constants.
//!
//! Generated from the desktop UI stylesheet constants. Do not edit by hand.
//! Compare with [`crate::layout`] — tests fail when a constant drifts
//! from the Electron stylesheet.

#![allow(missing_docs)]

/// CSS `#main-tab` `width`.
pub const CSS_MAIN_TAB_W: f32 = 64.0;
/// CSS `#sidebarNav` `width`.
pub const CSS_SIDEBAR_W: f32 = 408.0;
/// CSS `#sidebarNav − #main-tab` `width`.
pub const CSS_CONV_LIST_INNER_W: f32 = 344.0;
/// CSS `body --right-side-width` `width`.
pub const CSS_RIGHT_SIDE_W: f32 = 344.0;
/// CSS `#contact-search` `height`.
pub const CSS_CONTACT_SEARCH_H: f32 = 64.0;
/// CSS `#contact-search` `pad_x`.
pub const CSS_CONTACT_SEARCH_PAD_X: f32 = 16.0;
/// CSS `#contact-search .group-search` `height`.
pub const CSS_GROUP_SEARCH_H: f32 = 32.0;
/// CSS `#contact-search-input` `radius`.
pub const CSS_SEARCH_INPUT_RADIUS: f32 = 5.0;
/// CSS `#contact-search .action-btn` `width`.
pub const CSS_SEARCH_ACTION_BTN: f32 = 30.0;
/// CSS `#contact-search .action-btn > i` `width`.
pub const CSS_SEARCH_ACTION_ICON: f32 = 22.0;
/// CSS `.nav__tabs__zalo` `height`.
pub const CSS_NAV_LOGO_H: f32 = 92.0;
/// CSS `.nav__tabs__zalo` `pad_t`.
pub const CSS_NAV_LOGO_PAD_TOP: f32 = 32.0;
/// CSS `.nav__tabs__zalo > i` `width`.
pub const CSS_NAV_LOGO_GLYPH: f32 = 43.0;
/// CSS `.nav__tabs__bottom__btn` `height`.
pub const CSS_NAV_BOTTOM_BTN_H: f32 = 60.0;
/// CSS `.leftbar-tab` `height`.
pub const CSS_LEFTBAR_TAB_H: f32 = 56.0;
/// CSS `.leftbar-tab .mmi-icon-wr` `radius`.
pub const CSS_LEFTBAR_MARK_RADIUS: f32 = 6.0;
/// CSS `header` `height`.
pub const CSS_CHAT_HEADER_H: f32 = 68.0;
/// CSS `.chat-box-input__tools` `height`.
pub const CSS_COMPOSER_TOOLS_H: f32 = 40.0;
/// CSS `.chat-box-input__tools__btn` `width`.
pub const CSS_COMPOSER_TOOL_BTN: f32 = 40.0;
/// CSS `.chat-box-input__tools__btn` `font_px`.
pub const CSS_COMPOSER_TOOL_ICON: f32 = 18.0;
/// CSS `body` `font_px`.
pub const CSS_FONT_BASE: f32 = 14.0;
/// CSS `body` `letter_spacing`.
pub const CSS_FONT_LETTER_SPACING: f32 = 0.2;
/// CSS `.conv-item` `mar_y`.
pub const CSS_CONV_ITEM_MARGIN_Y: f32 = 4.0;
/// CSS `.conv-item` `mar_x`.
pub const CSS_CONV_ITEM_MARGIN_X: f32 = 8.0;
/// CSS `.conv-item` `pad_x`.
pub const CSS_CONV_ITEM_PADDING_X: f32 = 8.0;
/// CSS `.conv-item` `radius`.
pub const CSS_CONV_ITEM_RADIUS: f32 = 6.0;
/// CSS `.login-qr-page` `pad_t`.
pub const CSS_LOGIN_PAGE_PAD_Y: f32 = 60.0;
/// CSS `.login-qr-page` `pad_l`.
pub const CSS_LOGIN_PAGE_PAD_X: f32 = 32.0;
/// CSS `.login-qr-page` `gap`.
pub const CSS_LOGIN_PAGE_GAP: f32 = 32.0;
/// CSS `.login-qr__frame-qr` `width`.
pub const CSS_LOGIN_QR_FRAME: f32 = 250.0;
/// CSS `.sticker-selector` `width`.
pub const CSS_STICKER_SELECTOR_W: f32 = 360.0;
/// CSS `.sticker-selector__menu` `height`.
pub const CSS_STICKER_MENU_H: f32 = 56.0;
/// CSS `#rtfTool .chat-box-input__tools__btn` `width`.
pub const CSS_RTF_TOOL_BTN: f32 = 30.0;

/// JS export `CHAT_WIDTH_LIM`.
pub const JS_CHAT_WIDTH_LIM: f32 = 520.0;
/// JS export `CONTACT_ITEM_HEIGHT`.
pub const JS_CONTACT_ITEM_HEIGHT: f32 = 72.0;
/// JS export `CONTACT_LABEL_HEIGHT`.
pub const JS_CONTACT_LABEL_HEIGHT: f32 = 47.0;
/// JS export `CONVERSATION_ITEM_HEIGHT`.
pub const JS_CONVERSATION_ITEM_HEIGHT: f32 = 78.0;
/// JS export `MAX_AUTO_DOWNLOAD_SIZE`.
pub const JS_MAX_AUTO_DOWNLOAD_SIZE: f32 = 10485760.0;
/// JS export `MAX_EXCEL_PREVIEW_SIZE`.
pub const JS_MAX_EXCEL_PREVIEW_SIZE: f32 = 5242880.0;
/// JS export `MAX_PDF_PREVIEW_SIZE`.
pub const JS_MAX_PDF_PREVIEW_SIZE: f32 = 15728640.0;
/// JS export `MIN_WIDTH_WINDOW_APP`.
pub const JS_MIN_WIDTH_WINDOW_APP: f32 = 580.0;
/// JS export `NOTI_ITEM_HEIGHT`.
pub const JS_NOTI_ITEM_HEIGHT: f32 = 68.0;
/// JS export `RESIZE_THRESHOLD`.
pub const JS_RESIZE_THRESHOLD: f32 = 2621440.0;
/// JS export `SEARCH_ITEM_HEIGHT`.
pub const JS_SEARCH_ITEM_HEIGHT: f32 = 72.0;
/// JS export `WIDTH_CHAT_INFO`.
pub const JS_WIDTH_CHAT_INFO: f32 = 340.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;

    #[test]
    fn css_matches_hand_layout() {
        assert_eq!(CSS_MAIN_TAB_W, layout::MAIN_TAB_W, "MAIN_TAB_W");
        assert_eq!(CSS_SIDEBAR_W, layout::SIDEBAR_W, "SIDEBAR_W");
        assert_eq!(CSS_CONV_LIST_INNER_W, layout::CONV_LIST_INNER_W, "CONV_LIST_INNER_W");
        assert_eq!(CSS_RIGHT_SIDE_W, layout::RIGHT_SIDE_W, "RIGHT_SIDE_W");
        assert_eq!(CSS_CONTACT_SEARCH_H, layout::CONTACT_SEARCH_H, "CONTACT_SEARCH_H");
        assert_eq!(CSS_CONTACT_SEARCH_PAD_X, layout::CONTACT_SEARCH_PAD_X, "CONTACT_SEARCH_PAD_X");
        assert_eq!(CSS_GROUP_SEARCH_H, layout::GROUP_SEARCH_H, "GROUP_SEARCH_H");
        assert_eq!(CSS_SEARCH_INPUT_RADIUS, layout::SEARCH_INPUT_RADIUS, "SEARCH_INPUT_RADIUS");
        assert_eq!(CSS_SEARCH_ACTION_BTN, layout::SEARCH_ACTION_BTN, "SEARCH_ACTION_BTN");
        assert_eq!(CSS_SEARCH_ACTION_ICON, layout::SEARCH_ACTION_ICON, "SEARCH_ACTION_ICON");
        assert_eq!(CSS_NAV_LOGO_H, layout::NAV_LOGO_H, "NAV_LOGO_H");
        assert_eq!(CSS_NAV_LOGO_PAD_TOP, layout::NAV_LOGO_PAD_TOP, "NAV_LOGO_PAD_TOP");
        assert_eq!(CSS_NAV_LOGO_GLYPH, layout::NAV_LOGO_GLYPH, "NAV_LOGO_GLYPH");
        assert_eq!(CSS_NAV_BOTTOM_BTN_H, layout::NAV_BOTTOM_BTN_H, "NAV_BOTTOM_BTN_H");
        assert_eq!(CSS_LEFTBAR_TAB_H, layout::LEFTBAR_TAB_H, "LEFTBAR_TAB_H");
        assert_eq!(CSS_LEFTBAR_MARK_RADIUS, layout::LEFTBAR_MARK_RADIUS, "LEFTBAR_MARK_RADIUS");
        assert_eq!(CSS_CHAT_HEADER_H, layout::CHAT_HEADER_H, "CHAT_HEADER_H");
        assert_eq!(CSS_COMPOSER_TOOLS_H, layout::COMPOSER_TOOLS_H, "COMPOSER_TOOLS_H");
        assert_eq!(CSS_COMPOSER_TOOL_BTN, layout::COMPOSER_TOOL_BTN, "COMPOSER_TOOL_BTN");
        assert_eq!(CSS_COMPOSER_TOOL_ICON, layout::COMPOSER_TOOL_ICON, "COMPOSER_TOOL_ICON");
        assert_eq!(CSS_FONT_BASE, layout::FONT_BASE, "FONT_BASE");
        assert_eq!(CSS_FONT_LETTER_SPACING, layout::FONT_LETTER_SPACING, "FONT_LETTER_SPACING");
        assert_eq!(CSS_CONV_ITEM_MARGIN_Y, layout::CONV_ITEM_MARGIN_Y, "CONV_ITEM_MARGIN_Y");
        assert_eq!(CSS_CONV_ITEM_MARGIN_X, layout::CONV_ITEM_MARGIN_X, "CONV_ITEM_MARGIN_X");
        assert_eq!(CSS_CONV_ITEM_PADDING_X, layout::CONV_ITEM_PADDING_X, "CONV_ITEM_PADDING_X");
        assert_eq!(CSS_CONV_ITEM_RADIUS, layout::CONV_ITEM_RADIUS, "CONV_ITEM_RADIUS");
        assert_eq!(CSS_LOGIN_PAGE_PAD_Y, layout::LOGIN_PAGE_PAD_Y, "LOGIN_PAGE_PAD_Y");
        assert_eq!(CSS_LOGIN_PAGE_PAD_X, layout::LOGIN_PAGE_PAD_X, "LOGIN_PAGE_PAD_X");
        assert_eq!(CSS_LOGIN_PAGE_GAP, layout::LOGIN_PAGE_GAP, "LOGIN_PAGE_GAP");
        assert_eq!(CSS_LOGIN_QR_FRAME, layout::LOGIN_QR_FRAME, "LOGIN_QR_FRAME");
        assert_eq!(CSS_STICKER_SELECTOR_W, layout::STICKER_SELECTOR_W, "STICKER_SELECTOR_W");
        assert_eq!(CSS_STICKER_MENU_H, layout::STICKER_MENU_H, "STICKER_MENU_H");
        assert_eq!(CSS_RTF_TOOL_BTN, layout::RTF_TOOL_BTN, "RTF_TOOL_BTN");
        assert_eq!(JS_CONVERSATION_ITEM_HEIGHT, layout::CONV_ITEM_H);
        assert_eq!(JS_CONTACT_ITEM_HEIGHT, layout::CONTACT_ITEM_H);
    }
}
