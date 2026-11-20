//! Measured Zalo desktop layout constants.
//!
//! Every number below is taken from the decoded CSS or from a real window capture.

/// Login window size measured with Xvfb (window title Zalo): 420x644.
pub const LOGIN_WINDOW: (f32, f32) = (420.0, 644.0);
/// `.login-qr-page` vertical padding (`60px 32px`).
pub const LOGIN_PAGE_PAD_Y: f32 = 60.0;
/// `.login-qr-page` horizontal padding.
pub const LOGIN_PAGE_PAD_X: f32 = 32.0;
/// `.login-qr-page` column gap.
pub const LOGIN_PAGE_GAP: f32 = 32.0;
/// `.login-qr-page .title` gap (logo 32 px then title).
pub const LOGIN_TITLE_GAP: f32 = 16.0;
/// Logo image height on the QR page (`style.height: 32px`).
pub const LOGIN_LOGO_H: f32 = 32.0;
/// Title line box (`font-size: 18px`, `line-height: 24px`).
pub const LOGIN_TITLE_H: f32 = 24.0;
/// `.login-qr__frame-qr` box.
pub const LOGIN_QR_FRAME: f32 = 250.0;
/// QR bitmap measured from the desktop login screen capture (420x644).
pub const LOGIN_QR_IMAGE: f32 = 240.0;
/// `.login-qr-page .qr-scan` gap (frame, error, instructions).
pub const LOGIN_QR_SCAN_GAP: f32 = 16.0;
/// Error line under the QR (`minHeight: 20px`).
pub const LOGIN_QR_ERROR_H: f32 = 20.0;
/// `.login-qr-page .instructions` gap and 14/20 line box.
pub const LOGIN_INSTR_GAP: f32 = 12.0;
/// Instruction line height (`font-size: 14px`, `line-height: 20px`).
pub const LOGIN_INSTR_H: f32 = 20.0;
/// `.login-qr-page .instructions` max-width.
pub const LOGIN_INSTR_MAX_W: f32 = 331.0;
/// Three instruction lines (`3 × 20 + 2 × 12`).
pub const LOGIN_GUIDES_H: f32 = 3.0 * LOGIN_INSTR_H + 2.0 * LOGIN_INSTR_GAP;

/// Vertical slots of `.login-qr-page`, fitted into a window height.
///
/// CSS uses a 420×644 window. Wayland with fractional scale (KDE 1.35)
/// often gives a shorter inner rect (~522). This stack keeps the three
/// guides inside that rect by shrinking padding before the QR frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoginPageLayout {
    /// `.login-qr-page` vertical padding after shrink-to-fit.
    pub pad_y: f32,
    /// `.login-qr-page` horizontal padding.
    pub pad_x: f32,
    /// Gap between logo and title.
    pub title_gap: f32,
    /// Gap between the title block and the QR frame.
    pub page_gap: f32,
    /// Gap around the error slot (frame → error → instructions).
    pub scan_gap: f32,
    /// Error line box. Zero when the empty slot is collapsed.
    pub error_h: f32,
    /// QR frame edge (CSS 250, smaller only on a very short window).
    pub qr_frame: f32,
    /// Top of the logo, relative to the window (below the 38 px bar).
    pub logo_y: f32,
    /// Top of the title line.
    pub title_y: f32,
    /// Top of the QR frame.
    pub qr_y: f32,
    /// Top of the error line.
    pub error_y: f32,
    /// Top of instruction 1.
    pub guides_y: f32,
}

impl LoginPageLayout {
    /// Bottom of the third instruction line.
    pub fn guides_bottom(&self) -> f32 {
        self.guides_y + LOGIN_GUIDES_H
    }
}

fn login_stack_h(
    pad_y: f32,
    title_gap: f32,
    page_gap: f32,
    scan_gap: f32,
    error_h: f32,
    qr_frame: f32,
) -> f32 {
    let between = if error_h > 0.5 {
        scan_gap + error_h + scan_gap
    } else {
        scan_gap
    };
    LINUX_TITLEBAR_H
        + pad_y
        + LOGIN_LOGO_H
        + title_gap
        + LOGIN_TITLE_H
        + page_gap
        + qr_frame
        + between
        + LOGIN_GUIDES_H
        + pad_y
}

fn shrink_slot(value: &mut f32, min: f32, overflow: &mut f32, copies: f32) {
    if *overflow <= 0.5 {
        return;
    }
    let room = (*value - min) * copies;
    if room <= 0.0 {
        return;
    }
    let take = room.min(*overflow);
    *value -= take / copies;
    *overflow -= take;
}

/// Stack the QR login page so the three guides stay inside `window_h`.
///
/// At 644 px this matches the CSS padding and the 250 px frame. On a
/// shorter inner size the empty error slot and the 60 px padding shrink
/// first; the QR frame shrinks last.
pub fn login_page_layout(window_h: f32, has_error: bool) -> LoginPageLayout {
    let window_h = window_h.max(LINUX_TITLEBAR_H + LOGIN_GUIDES_H + 96.0);
    let mut pad_y = LOGIN_PAGE_PAD_Y;
    let mut title_gap = LOGIN_TITLE_GAP;
    let mut page_gap = LOGIN_PAGE_GAP;
    let mut scan_gap = LOGIN_QR_SCAN_GAP;
    let mut error_h = LOGIN_QR_ERROR_H;
    let mut qr_frame = LOGIN_QR_FRAME;

    let mut overflow =
        login_stack_h(pad_y, title_gap, page_gap, scan_gap, error_h, qr_frame) - window_h;
    if overflow > 0.5 && !has_error {
        error_h = 0.0;
        overflow =
            login_stack_h(pad_y, title_gap, page_gap, scan_gap, error_h, qr_frame) - window_h;
    }
    shrink_slot(&mut pad_y, 16.0, &mut overflow, 2.0);
    shrink_slot(&mut page_gap, 8.0, &mut overflow, 1.0);
    shrink_slot(&mut scan_gap, 8.0, &mut overflow, 2.0);
    shrink_slot(&mut title_gap, 8.0, &mut overflow, 1.0);
    shrink_slot(&mut qr_frame, 180.0, &mut overflow, 1.0);
    shrink_slot(&mut pad_y, 8.0, &mut overflow, 2.0);
    shrink_slot(&mut qr_frame, 120.0, &mut overflow, 1.0);

    let mut y = LINUX_TITLEBAR_H + pad_y;
    let logo_y = y;
    y += LOGIN_LOGO_H + title_gap;
    let title_y = y;
    y += LOGIN_TITLE_H + page_gap;
    let qr_y = y;
    y += qr_frame;
    let (error_y, guides_y) = if error_h > 0.5 {
        y += scan_gap;
        let error_y = y;
        y += error_h + scan_gap;
        (error_y, y)
    } else {
        let error_y = y;
        y += scan_gap;
        (error_y, y)
    };
    let max_guides_y = (window_h - pad_y.min(12.0) - LOGIN_GUIDES_H).max(qr_y + qr_frame);
    let guides_y = guides_y.min(max_guides_y);

    LoginPageLayout {
        pad_y,
        pad_x: LOGIN_PAGE_PAD_X,
        title_gap,
        page_gap,
        scan_gap,
        error_h,
        qr_frame,
        logo_y,
        title_y,
        qr_y,
        error_y,
        guides_y,
    }
}

/// #main-tab width: the 64 px icon rail on the far left.
pub const MAIN_TAB_W: f32 = 64.0;
/// #sidebarNav default width (`var(--nav-width)`). Includes `#main-tab`.
pub const SIDEBAR_W: f32 = 408.0;
/// Conversation list inside `#sidebarNav` (`408 − 64`). Same as `--right-side-width`.
pub const CONV_LIST_INNER_W: f32 = 344.0;
/// #sidebarNav.compact-mode.show-chat width.
pub const SIDEBAR_COMPACT_W: f32 = 64.0;
/// `--right-side-width`: compact chat-info panel.
pub const RIGHT_SIDE_W: f32 = 344.0;

/// .titlebar height in the web bundle (the Linux wrapper adds its own 38 px bar).
pub const TITLEBAR_H: f32 = 24.0;
/// Linux wrapper title bar injected through inline CSS (#titleBar).
pub const LINUX_TITLEBAR_H: f32 = 38.0;
/// header#header height in the wrapper child-window CSS.
pub const HEADER_H: f32 = 54.0;
/// Main-window `header { height: 68px; padding: 0 16px }`.
pub const CHAT_HEADER_H: f32 = 68.0;

/// .nav__tabs__zalo block height (logo area).
pub const NAV_LOGO_H: f32 = 92.0;
/// `.nav__tabs__zalo { padding-top: 32px }`.
pub const NAV_LOGO_PAD_TOP: f32 = 32.0;
/// .nav__tabs__zalo > i logo glyph box (`2.6875rem`).
pub const NAV_LOGO_GLYPH: f32 = 43.0;
/// .nav__tabs__bottom__btn height.
pub const NAV_BOTTOM_BTN_H: f32 = 60.0;
/// `.leftbar-tab { height: 56px }`.
pub const LEFTBAR_TAB_H: f32 = 56.0;
/// `.mmi-icon-wr { border-radius: 6px }`.
pub const LEFTBAR_MARK_RADIUS: f32 = 6.0;
/// `.leftbar-tab .internal-icon { font-size: 1.75rem }`.
pub const LEFTBAR_ICON_PX: f32 = 28.0;
/// `.nav__tabs__bottom__btn__menu { font-size: 1.5rem }`.
pub const NAV_BOTTOM_ICON_PX: f32 = 24.0;
/// `#contact-search .group-search__icon { font-size: 0.875rem }`.
pub const SEARCH_FIELD_ICON: f32 = 14.0;
/// Main chat header action glyph (24px faces, shell paints 20).
pub const HEADER_ACTION_ICON: f32 = 20.0;
/// Header action hit box (`32×32`, radius 6).
pub const HEADER_ACTION_BTN: f32 = 32.0;
/// `.card { border-radius: 8px }` — bubble / modal / sticker.
pub const CARD_RADIUS: f32 = 8.0;
/// `.chat-date > span { border-radius: 10px }`.
pub const CHAT_DATE_RADIUS: f32 = 10.0;
/// Hover chip on `.chat-box-input__tools__btn`.
pub const COMPOSER_TOOL_RADIUS: f32 = 8.0;
/// CSD clip of cards. The Linux window frame uses [`LINUX_FRAME_RADIUS`].
pub const WINDOW_RADIUS: f32 = 8.0;
/// `preload-child-linux.js` body radius. Not the 8 px card radius.
pub const LINUX_FRAME_RADIUS: f32 = 12.0;
/// Transparent CSD inset so compositor shows rounded corners + shadow.
pub const WINDOW_SHADOW: f32 = 8.0;
/// `.context-menu { border-radius: 4px }`.
pub const CONTEXT_MENU_RADIUS: f32 = 4.0;
/// `.context-menu { min-width: 110px }`.
pub const CONTEXT_MENU_MIN_W: f32 = 110.0;
/// `.context-menu { max-width: 300px }`.
pub const CONTEXT_MENU_MAX_W: f32 = 300.0;
/// `.context-menu { font-size: 0.8125rem }`.
pub const CONTEXT_MENU_FONT: f32 = 13.0;
/// `.context-menu { padding: 4px 0 }`.
pub const CONTEXT_MENU_PAD_Y: f32 = 4.0;
/// `.context-menu-item` top padding.
pub const CONTEXT_MENU_ITEM_PAD_T: f32 = 6.0;
/// `.context-menu-item` right padding.
pub const CONTEXT_MENU_ITEM_PAD_R: f32 = 22.0;
/// `.context-menu-item` bottom padding.
pub const CONTEXT_MENU_ITEM_PAD_B: f32 = 3.0;
/// `.context-menu-item` left padding.
pub const CONTEXT_MENU_ITEM_PAD_L: f32 = 14.0;

/// `#contact-search { height: 64px; padding: 0 16px }`.
pub const CONTACT_SEARCH_H: f32 = 64.0;
/// Horizontal padding of `#contact-search`.
pub const CONTACT_SEARCH_PAD_X: f32 = 16.0;
/// `#contact-search .group-search` / `#contact-search-input` height.
pub const GROUP_SEARCH_H: f32 = 32.0;
/// `#contact-search-input { border-radius: 5px }`.
pub const SEARCH_INPUT_RADIUS: f32 = 5.0;
/// `#contact-search .action-btn` (Add friend / Create group).
pub const SEARCH_ACTION_BTN: f32 = 30.0;
/// `#contact-search .action-btn > i` (`1.375rem`).
pub const SEARCH_ACTION_ICON: f32 = 22.0;

/// `CONVERSATION_ITEM_HEIGHT` in compact0293.js.
pub const CONV_ITEM_H: f32 = 78.0;
/// `CONTACT_ITEM_HEIGHT` in compact0293.js.
pub const CONTACT_ITEM_H: f32 = 72.0;
/// .conv-item vertical margin (4 px top + 4 px bottom around a virtual row).
pub const CONV_ITEM_MARGIN_Y: f32 = 4.0;
/// .conv-item horizontal margin.
pub const CONV_ITEM_MARGIN_X: f32 = 8.0;
/// .conv-item inner horizontal padding.
pub const CONV_ITEM_PADDING_X: f32 = 8.0;
/// .conv-item corner radius.
pub const CONV_ITEM_RADIUS: f32 = 6.0;

/// .chat-box-input__tools toolbar height.
pub const COMPOSER_TOOLS_H: f32 = 40.0;
/// .chat-box-input__tools__btn square button (later rule `40×40`).
pub const COMPOSER_TOOL_BTN: f32 = 40.0;
/// `.chat-box-input__tools__btn { font-size: 1.125rem }`.
pub const COMPOSER_TOOL_ICON: f32 = 18.0;

/// Base font size used by the web bundle (body 14 px, html 16 px).
pub const FONT_BASE: f32 = 14.0;
/// `body { letter-spacing: 0.2px }`.
pub const FONT_LETTER_SPACING: f32 = 0.2;
/// `.conv-title-name { font-size: 1rem }`.
pub const CONV_TITLE_PX: f32 = 16.0;
/// `.conv-message { font-size: 0.875rem }`.
pub const CONV_PREVIEW_PX: f32 = 14.0;
/// `.conv-item-title__more { font-size: 0.75rem }`.
pub const CONV_TIME_PX: f32 = 12.0;
/// `.rich-input { font-size: 0.9375rem }`.
pub const RICH_INPUT_PX: f32 = 15.0;
/// `.sticker-selector { width: 360px }`.
pub const STICKER_SELECTOR_W: f32 = 360.0;
/// `.sticker-selector__menu { height: 56px }` — pack-icon strip at the bottom.
pub const STICKER_MENU_H: f32 = 56.0;
/// `.sticker-selector__title { height: 30px }` — STICKER / EMOJI / GIF tabs.
pub const STICKER_TITLE_H: f32 = 30.0;
/// `.sticker-selector__content__item { width/height: 65px }`.
pub const STICKER_ITEM: f32 = 65.0;
/// `.sticker-selector__content__item { padding: 3.5px }`.
pub const STICKER_ITEM_PAD: f32 = 3.5;
/// `.sticker-selector__menu__item > img` 30×30.
pub const STICKER_PACK_ICON: f32 = 30.0;
/// Left/right margin of a pack icon (`7px`).
pub const STICKER_PACK_ICON_MARGIN: f32 = 7.0;
/// `.card.card--file { width: 376px }`.
pub const FILE_CARD_W: f32 = 376.0;
/// `#rtfTool .chat-box-input__tools__btn` (`30×30`, not the 40 px composer tools).
pub const RTF_TOOL_BTN: f32 = 30.0;
/// Zalo font family name declared in @font-face.
pub const FONT_FAMILY: &str = "zalo-font";
/// Segoe UI family used for Vietnamese text.
pub const FONT_FAMILY_VN: &str = "SegoeuiPc";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_adds_up() {
        assert_eq!(MAIN_TAB_W + CONV_LIST_INNER_W, SIDEBAR_W);
        assert_eq!(CONV_LIST_INNER_W, RIGHT_SIDE_W);
        assert_eq!(CONTACT_SEARCH_H, 64.0);
        assert_eq!(GROUP_SEARCH_H, 32.0);
        assert_eq!(CHAT_HEADER_H, 68.0);
        assert_eq!(COMPOSER_TOOL_ICON, 18.0);
        assert_eq!(FONT_LETTER_SPACING, 0.2);
        assert_eq!(LOGIN_WINDOW, (420.0, 644.0));
        assert_eq!(LOGIN_QR_FRAME, 250.0);
        assert_eq!(STICKER_SELECTOR_W, 360.0);
        assert_eq!(STICKER_MENU_H, 56.0);
        assert_eq!(STICKER_TITLE_H, 30.0);
        assert_eq!(STICKER_ITEM, 65.0);
        assert_eq!(STICKER_ITEM_PAD, 3.5);
        assert_eq!(STICKER_PACK_ICON, 30.0);
        assert_eq!(STICKER_PACK_ICON_MARGIN, 7.0);
        assert_eq!(FILE_CARD_W, 376.0);
        assert_eq!(RTF_TOOL_BTN, 30.0);
        assert_eq!(LEFTBAR_ICON_PX, 28.0);
        assert_eq!(NAV_BOTTOM_ICON_PX, 24.0);
        assert_eq!(SEARCH_FIELD_ICON, 14.0);
        assert_eq!(HEADER_ACTION_ICON, 20.0);
        assert_eq!(HEADER_ACTION_BTN, 32.0);
        assert_eq!(CARD_RADIUS, 8.0);
        assert_eq!(CHAT_DATE_RADIUS, 10.0);
        assert_eq!(COMPOSER_TOOL_RADIUS, 8.0);
        assert_eq!(WINDOW_RADIUS, CARD_RADIUS);
        assert_eq!(LINUX_FRAME_RADIUS, 12.0);
        assert_eq!(WINDOW_SHADOW, 8.0);
        assert_eq!(CONTEXT_MENU_RADIUS, 4.0);
        assert_eq!(CONTEXT_MENU_MIN_W, 110.0);
        assert_eq!(CONTEXT_MENU_MAX_W, 300.0);
        assert_eq!(CONTEXT_MENU_FONT, 13.0);
        assert_eq!(LOGIN_QR_IMAGE, 240.0);
        assert_eq!(LOGIN_PAGE_PAD_Y, 60.0);
        assert_eq!(LOGIN_PAGE_PAD_X, 32.0);
        assert_eq!(LOGIN_INSTR_MAX_W, 331.0);
        assert_eq!(LOGIN_GUIDES_H, 84.0);
    }

    #[test]
    fn login_guides_fit_measured_and_short_windows() {
        for (height, has_error) in [
            (LOGIN_WINDOW.1, false),
            (LOGIN_WINDOW.1, true),
            (522.0, false),
            (522.0, true),
            (480.0, false),
        ] {
            let layout = login_page_layout(height, has_error);
            assert!(
                layout.guides_bottom() <= height + 0.5,
                "h={height} err={has_error} bottom={}",
                layout.guides_bottom()
            );
            assert!(
                layout.guides_y + 0.5 >= layout.qr_y + layout.qr_frame,
                "guides overlap QR at h={height}"
            );
            assert!(layout.qr_frame >= 120.0);
            if has_error {
                assert!(layout.error_h >= LOGIN_QR_ERROR_H - 0.5);
            }
        }
        let full = login_page_layout(LOGIN_WINDOW.1, false);
        assert!((full.qr_frame - LOGIN_QR_FRAME).abs() < 1.0);
        assert!(full.pad_y >= 50.0);
        let short = login_page_layout(522.0, false);
        assert_eq!(short.error_h, 0.0);
        assert!(short.qr_frame >= 200.0);
        assert!(short.guides_y + LOGIN_INSTR_H * 3.0 + LOGIN_INSTR_GAP * 2.0 <= 522.5);
    }
}
