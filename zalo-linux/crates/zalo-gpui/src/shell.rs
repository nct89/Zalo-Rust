//! Main window: `#main-tab` 64 full height + gray titlebar + list 344 + chat.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{
    div, prelude::*, px, rgb, Context, Entity, IntoElement, ParentElement, Render, RenderImage,
    SharedString, Styled, Window,
};
use std::sync::{Arc, Mutex};

use crate::bridge::{self, LiveAction};
use crate::color::{str_vi, token_rgb};
use crate::context_menu::OpenMenu;
use crate::data::{self, ChatConv, ChatMsg, ChatPerson};
use crate::line_input::{LineEvent, LineInput};
use crate::preview;
use crate::rail::Rail;
use crate::settings::SettingsPage;
use crate::sidebar::ContactTab;
use crate::stickers::{StickerState, StickerTab};
use crate::titlebar::main_titlebar_with_theme;
use crate::window_frame::{wrap_csd, CsdCorners};

/// Default windowed size for the preview shell (~Zalo PC).
pub const MAIN_WINDOW_DEFAULT: (f32, f32) = (1200.0, 720.0);
/// Height floor; width floor is `JS_MIN_WIDTH_WINDOW_APP` 580.
pub const MAIN_WINDOW_MIN_H: f32 = 400.0;

/// `g_<id>` ↔ bare id. Group profiles are stored with the prefix.
fn alt_profile_key(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("g_") {
        rest.to_owned()
    } else {
        format!("g_{key}")
    }
}

fn notice_wake() -> &'static Mutex<Option<gpui::WeakEntity<ShellView>>> {
    static WAKE: std::sync::OnceLock<Mutex<Option<gpui::WeakEntity<ShellView>>>> =
        std::sync::OnceLock::new();
    WAKE.get_or_init(|| Mutex::new(None))
}

fn bind_notice_wake(weak: gpui::WeakEntity<ShellView>) {
    if let Ok(mut guard) = notice_wake().lock() {
        *guard = Some(weak);
    }
}

/// Ask the main shell to store a dragged notice position. Called from the popup.
pub(crate) fn poke_notice_wake(cx: &mut gpui::App) {
    let weak = notice_wake().lock().ok().and_then(|guard| guard.clone());
    if let Some(weak) = weak {
        let _ = weak.update(cx, |view, cx| {
            view.apply_notice_custom();
            cx.notify();
        });
    }
}

/// Focus the main Zalo window and open the conversation from a clicked notice.
pub(crate) fn open_notice_thread(thread_key: &str, cx: &mut gpui::App) {
    let key = thread_key.to_owned();
    crate::show_main_window(cx);
    cx.activate(true);
    let weak = notice_wake().lock().ok().and_then(|guard| guard.clone());
    if let Some(weak) = weak {
        let _ = weak.update(cx, |view, cx| {
            if let Some(id) = view
                .convs
                .iter()
                .find(|c| c.thread_id == key)
                .map(|c| c.id)
            {
                view.select_conv(id);
                cx.notify();
            }
        });
    }
}

/// Avatar URL for `key`, also trying the `g_` form.
fn profile_avatar(profiles: &crate::profiles::Profiles, key: &str) -> Option<String> {
    profiles
        .map
        .get(key)
        .or_else(|| profiles.map.get(&alt_profile_key(key)))
        .map(|p| p.avatar.clone())
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
}

/// Zalo PC responsive shell modes (`verifyCompactMode` in `compact-app-pc.*.js`):
/// - `Normal` (`width > 1224`): Rail (64) + Sidebar (344) + Chat + Docked Info (344).
/// - `SemiCompact` (`880 < width <= 1224`): Rail (64) + Sidebar (344) + Chat; Info floats.
/// - `Compact` (`width <= 880`): Rail (64) + single active pane (either full-width Sidebar
///   OR full-width Chat with `<` back button in header, matching `#sidebarNav.compact-mode` /
///   `#sidebarNav.compact-mode.show-chat` and `main.compact-mode { display: none }`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompactLevel {
    #[default]
    Normal,
    SemiCompact,
    Compact,
}

impl CompactLevel {
    /// Exact Zalo PC breakpoint below which the shell hides the list when viewing chat.
    pub const COMPACT_MAX_W: f32 = 880.0;
    /// Exact Zalo PC breakpoint above which the shell is in full desktop mode.
    pub const SEMI_COMPACT_MAX_W: f32 = 1224.0;

    #[must_use]
    pub fn for_width(width: f32, solo: bool) -> Self {
        if solo || width > Self::SEMI_COMPACT_MAX_W {
            Self::Normal
        } else if width > Self::COMPACT_MAX_W {
            Self::SemiCompact
        } else {
            Self::Compact
        }
    }
}

/// Active conversation's unread anchor (official Zalo PC `unreadMsgHandler` +
/// `unread-separator-container` + top-right `unread-floating-btn-container`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConvUnreadAnchor {
    /// Conversation element id (`ChatConv.id`).
    pub conv_id: u64,
    /// First unread message's local `ChatMsg.id` (divider renders directly above it).
    pub first_unread_msg_id: u64,
    /// Unread incoming message `ChatMsg.id`s (oldest -> newest) still above the viewport.
    pub remaining_ids: Vec<u64>,
    /// Original unread count when opened (shown on the unread separator divider).
    pub initial_count: u32,
}

/// Returns true when `text` mentions `@All`, `@Tất cả`, or `@<self_name>`.
pub(crate) fn message_mentions_me(text: &str, self_name: Option<&str>) -> bool {
    if !text.contains('@') {
        return false;
    }
    let known: Vec<String> = self_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| vec![s.to_owned()])
        .unwrap_or_default();
    let me_lower = self_name.map(|s| s.trim().to_lowercase());
    for span in crate::chat_select::find_mention_spans(text, &known) {
        let token = text[span].trim_start_matches('@').trim().to_lowercase();
        if token == "all" || token == "tất cả" || me_lower.as_deref() == Some(token.as_str()) {
            return true;
        }
    }
    false
}

/// Root view of the main shell.
pub struct ShellView {
    pub(crate) rail: Rail,
    pub(crate) list_filter: ListFilter,
    /// "Other" tab (archived threads) instead of "Focused".
    pub(crate) other_tab: bool,
    /// Conversation labels from `/api/convlabel/get` (loaded on first menu).
    pub(crate) labels: Vec<zalo_client::ConversationLabel>,
    pub(crate) labels_state: LabelsState,
    pub(crate) labels_pending: bool,
    /// Local label override per conversation.
    pub(crate) conv_labels: HashMap<u64, i64>,
    /// "Star Message".
    pub(crate) starred: HashSet<u64>,
    /// Left sidebar is showing the "Starred Messages" view (like Zalo PC).
    pub(crate) starred_open: bool,
    /// "Pin Message" (local).
    pub(crate) pinned_msgs: HashSet<u64>,
    pub(crate) selected: Option<u64>,
    pub(crate) contact_tab: ContactTab,
    pub(crate) note: Option<String>,
    /// When the current toast was first painted (auto-hide timer armed).
    note_at: Option<std::time::Instant>,
    /// Message popup waiting for the next frame. It opens a screen window.
    pending_notice: Option<crate::notice::NoticeContent>,
    /// Thread of the popup on screen, so a late avatar decode can fill it.
    pub(crate) notice_thread: Option<String>,
    /// Decoded chat wallpaper `(path, image)`.
    pub(crate) wall_image: RefCell<Option<(String, Arc<RenderImage>)>>,
    /// Composer had a selection when its context menu opened.
    pub(crate) input_has_sel: bool,
    /// Info column "Members" page.
    pub(crate) members_view: Option<crate::info::MembersView>,
    /// "Select Multiple Messages": conversation + ticked message ids.
    pub(crate) select_msgs: Option<(u64, Vec<u64>)>,
    /// Drag-selected text range inside the active chat thread.
    pub(crate) chat_sel: crate::chat_select::ChatTextSelection,
    /// Per-frame layout registry for selectable message text blocks.
    pub(crate) chat_sel_reg: crate::chat_select::SharedSelectionRegistry,
    /// Link card previews by URL (`parselink` / realtime), disk-cached.
    pub(crate) link_previews: HashMap<String, crate::links::LinkPreview>,
    /// URLs whose preview or thumbnail is being fetched.
    link_pending: HashSet<String>,
    /// Decoded link thumbnails by preview thumb URL.
    pub(crate) link_thumbs: HashMap<String, Arc<RenderImage>>,
    /// URL in the composer draft that `.chat-input-link-preview` is showing.
    pub(crate) composer_link: Option<String>,
    /// Preview the user closed; hidden until the draft URL changes.
    pub(crate) composer_link_hide: Option<String>,
    /// Poll ids whose detail fetch is in flight.
    poll_pending: HashSet<i64>,
    /// Poll ids whose detail fetch failed this session.
    poll_failed: HashSet<i64>,
    /// Voice message being played (`ffplay` child).
    pub(crate) voice_playing: Option<(u64, crate::audio_play::AudioHandle)>,
    /// Contacts search field and its text.
    pub(crate) contacts_query: Entity<LineInput>,
    pub(crate) contacts_q: String,
    /// Friends sorted Z→A.
    pub(crate) contacts_desc: bool,
    /// Groups sorted by name instead of activity.
    pub(crate) contacts_by_name: bool,
    /// Active label filter on Contacts (`None` = All).
    pub(crate) contacts_label: Option<i64>,
    /// Recent phone search results shown under "Recent Results" in "Add Friend".
    pub(crate) recent_phone_searches: Vec<crate::dialogs::Person>,
    /// "Friend Requests" page.
    pub(crate) requests: Option<crate::contacts::Requests>,
    /// "Group & Community Invitations" page.
    pub(crate) invites: Option<crate::contacts::Invites>,
    /// In-chat message search drawer state (`hdr-search`, matching Electron).
    pub(crate) msg_search: Entity<LineInput>,
    pub(crate) msg_search_open: bool,
    pub(crate) msg_search_q: String,
    pub(crate) msg_search_hits: Vec<u64>,
    pub(crate) msg_search_idx: usize,
    /// Optional sender filter (`None` = all, `Some("__me__")` = You, else sender key).
    pub(crate) msg_search_sender: Option<String>,
    pub(crate) msg_search_sender_label: String,
    /// Optional inclusive date range filter ("From date" ..= "To date").
    pub(crate) msg_search_date_from: Option<crate::calendar::CalDate>,
    pub(crate) msg_search_date_to: Option<crate::calendar::CalDate>,
    /// Open filter popover inside in-chat search (`Some("sender")` / `Some("date")`).
    pub(crate) msg_search_popup: Option<&'static str>,
    /// Draft start/end dates inside the date-range picker before clicking "Confirm".
    pub(crate) msg_search_draft_from: Option<crate::calendar::CalDate>,
    pub(crate) msg_search_draft_to: Option<crate::calendar::CalDate>,
    /// Active date slot inside the range picker (`0` = From date, `1` = To date).
    pub(crate) msg_search_draft_slot: u8,
    /// Displayed `(year, month)` inside the in-chat search calendar.
    pub(crate) msg_search_cal_ym: (u32, u8),
    /// Whether "Suggested time" preset list is expanded inside the date picker.
    pub(crate) msg_search_presets_open: bool,
    /// Account hidden chats (PIN md5, unlocked this run).
    pub(crate) hidden_srv: crate::privacy::Hidden,
    /// Screen lock is up.
    pub(crate) locked: bool,
    pub(crate) lock_error: bool,
    pub(crate) lock_input: Entity<LineInput>,
    /// Last unread total sent to the tray / launcher badge.
    badge_sent: Option<i64>,
    /// Root focus so Esc and Ctrl+C/Cmd+C reach the shell when no input is focused.
    pub(crate) root_focus: gpui::FocusHandle,
    pub(crate) settings_open: bool,
    pub(crate) settings_page: SettingsPage,
    pub(crate) sticker_open: bool,
    pub(crate) sticker_tab: StickerTab,
    pub(crate) english: bool,
    pub(crate) show_all_friends: bool,
    pub(crate) menu: Option<OpenMenu>,
    pub(crate) pinned: HashSet<u64>,
    pub(crate) muted: HashSet<u64>,
    pub(crate) archived: HashSet<u64>,
    pub(crate) hidden: HashSet<u64>,
    pub(crate) removed: HashSet<u64>,
    /// Friend request uids sent from the stranger header banner this session.
    pub(crate) sent_friend_reqs: HashSet<String>,
    pub(crate) marked_read: HashSet<u64>,
    pub(crate) reply_to: Option<ChatMsg>,
    pub(crate) convs: Vec<ChatConv>,
    pub(crate) messages: HashMap<u64, Vec<ChatMsg>>,
    pub(crate) friends: Vec<ChatPerson>,
    pub(crate) groups: Vec<ChatPerson>,
    pub(crate) search: Entity<LineInput>,
    pub(crate) composer: Entity<LineInput>,
    pub(crate) search_query: String,
    pub(crate) info_open: bool,
    /// Reopen the docked column when the window is wide (`showRightSidebar`).
    pub(crate) info_pref: bool,
    /// Last width class, so shrinking closes the overlay and widening restores it.
    pub(crate) info_wide: Option<bool>,
    /// Current responsive breakpoint (`Normal` / `SemiCompact` / `Compact`), matching
    /// Zalo PC `verifyCompactMode`.
    pub(crate) compact_level: CompactLevel,
    /// In `Compact` mode (`width <= 880`), `true` hides the conversation list column
    /// (`#sidebarNav.compact-mode.show-chat { width: 64px }`) and gives the active chat
    /// or contacts page the entire `width - 64px` canvas; `false` shows the full-width
    /// conversation list (`main.compact-mode { display: none }`).
    pub(crate) compact_show_chat: bool,
    /// Live logical width of the conversation column (`#messageViewContainer`),
    /// updated each frame from the window viewport minus rail, sidebar, and any
    /// docked right panel so bubbles/albums/floating actions scale without clipping.
    pub(crate) chat_pane_w: f32,
    /// Narrow window: the column floats and a click outside closes it.
    pub(crate) info_overlay: bool,
    /// Group invite link already fetched, keyed by conversation id.
    pub(crate) info_join: HashMap<u64, String>,
    pub(crate) info_join_busy: HashSet<u64>,
    /// Expanded Ảnh/Video (`0`), File (`1`) or Link (`2`) inside the info column.
    pub(crate) info_shelf: Option<(u64, u8)>,
    /// Composer capture includes the Zalo window.
    pub(crate) shot_with_zalo: bool,
    /// Photo-viewer drag origin `(mouse_x, mouse_y, pan_x, pan_y)`.
    pub(crate) viewer_drag: Option<(f32, f32, f32, f32)>,
    /// The pointer moved, so the click must not close the viewer.
    pub(crate) viewer_dragged: bool,
    pub(crate) forward_of: Option<ChatMsg>,
    pub(crate) display_name: Option<String>,
    pub(crate) uin: Option<String>,
    pub(crate) user_id: Option<String>,
    pub(crate) send2me_id: Option<String>,
    pub(crate) session_dir: Option<PathBuf>,
    pub(crate) has_session: bool,
    /// Server ended this PC session (`error_code` 600 / -69). Render opens QR.
    kick_to_login: bool,
    /// [`Self::leave_to_login`] already ran on this window.
    left_for_login: bool,
    pub(crate) next_msg_id: u64,
    /// Saved settings (`ui-state.json` + `gpui-prefs.json`).
    pub(crate) prefs: crate::prefs::Prefs,
    /// Last written `window` prefs (w, h, maximized) so render does not rewrite disk.
    window_saved: (i64, i64, bool),
    /// Disk sizes / audio devices for the open settings page.
    pub(crate) settings_cache: Option<crate::settings::SettingsCache>,
    /// Photos waiting in the composer tray under the text input.
    pub(crate) pending_photos: Vec<PathBuf>,
    pub(crate) pending_thumbs: Vec<Option<Arc<RenderImage>>>,
    /// Per-photo caption edited from the pencil on that thumb. Parallel to `pending_photos`.
    pub(crate) pending_captions: Vec<String>,
    /// Index of the thumb whose caption field is open.
    pub(crate) caption_edit: Option<usize>,
    pub(crate) caption_input: Entity<LineInput>,
    /// Region picker running.
    pub(crate) capture_busy: bool,
    /// zCloud / Tools / Starred Messages popover next to the rail.
    pub(crate) rail_popup: Option<crate::rail::RailPopup>,
    /// Newer GitHub Release available, if detected by `updater`.
    pub(crate) update_info: Option<crate::updater::UpdateInfo>,
    /// True while a release update check is currently in flight.
    pub(crate) update_checking: bool,
    /// Human-readable status note from the last manual update check.
    pub(crate) update_checked_note: Option<String>,
    /// Live download / verify / install state for in-app auto-update.
    pub(crate) update_install: crate::updater::UpdateInstallState,
    /// Tag for which the automatic update popup was already shown this session.
    pub(crate) update_popup_shown_tag: Option<String>,
    /// Bare group ids from `/api/group/getlg/e2ee`. `None` until a live fetch.
    pub(crate) e2ee_groups: Option<Vec<String>>,
    pub(crate) e2ee_busy: bool,
    pub(crate) e2ee_failed: bool,
    /// `(id, name)` from `/api/prodcatalog/catalog/list`.
    pub(crate) catalog_rows: Vec<(String, String)>,
    pub(crate) catalog_note: String,
    pub(crate) catalog_busy: bool,
    pub(crate) catalog_loaded: bool,
    pub(crate) media_bitmaps: HashMap<u64, Arc<RenderImage>>,
    /// Bubbles whose CDN thumb is queued or in flight.
    pub(crate) media_pending: HashSet<u64>,
    /// Bubbles whose CDN URL failed (expired 404 / undecodable).
    pub(crate) media_failed: HashSet<u64>,
    /// Jobs collected by [`Self::warm_media`]; drained in `render` (needs `cx`).
    pub(crate) media_queue: Vec<crate::fetch::Job>,
    /// Virtualized `#conv-scroll` handle (only visible conversation rows are rendered).
    pub(crate) conv_scroll: gpui::UniformListScrollHandle,
    /// Virtualized `#message-view` list state (`ListAlignment::Bottom`, only visible bubbles are rendered).
    pub(crate) msg_list: gpui::ListState,
    /// Last `(conv_id, item_count, hidden_count)` synced into `msg_list`.
    pub(crate) msg_list_state: std::cell::Cell<(Option<u64>, usize, usize)>,
    /// True when `#message-view` is scrolled up away from the newest bubble.
    pub(crate) chat_scrolled_up: bool,
    /// Live incoming messages received in the active chat while scrolled up.
    pub(crate) chat_unread_below: u32,
    /// Ordered local `ChatMsg.id`s of incoming messages below the viewport while scrolled up.
    pub(crate) new_below_ids: Vec<u64>,
    /// Subset of `new_below_ids` that `@mention` us or `@All` (drives the floating `@` jump button).
    pub(crate) new_mention_below_ids: Vec<u64>,
    /// Active conversation's unread divider + unread-above jump tracker.
    pub(crate) unread_anchor: Option<ConvUnreadAnchor>,
    /// Latest `(row_ix, msg_id)` mapping produced by `build_msg_rows` for the open chat.
    pub(crate) msg_row_ids: std::cell::RefCell<Vec<(usize, u64)>>,
    /// Row index of `MsgRowItem::UnreadDivider` in `msg_list`, if rendered.
    pub(crate) unread_divider_row: std::cell::Cell<Option<usize>>,
    /// Latest visible item range `(start, end)` reported by `msg_list`'s scroll handler.
    pub(crate) last_visible_range: std::cell::Cell<(usize, usize)>,
    /// Target message briefly highlighted after jumping via quote/search/pin/star/unread/mention.
    pub(crate) highlight_msg: Option<(u64, std::time::Instant)>,
    /// Debounce timestamp for auto-loading older messages when scrolling near top.
    pub(crate) last_auto_load_top: Option<std::time::Instant>,
    /// True when the multi-pin dropdown in the chat header is expanded.
    pub(crate) pinned_expanded: bool,
    /// Reaction detail modal `(msg_id, preset_filter)` (`None` = "Tất cả" tab).
    pub(crate) react_detail: Option<(u64, Option<usize>)>,
    /// Floating partial-quote / copy pill shown after selecting text inside a bubble.
    pub(crate) partial_quote_fab: Option<(u64, String, gpui::Point<gpui::Pixels>)>,
    /// Highlighted row in the floating vertical `@mention` popup.
    pub(crate) mention_idx: usize,
    /// Server-known pinned `thread_id`s preserved even before their conversation is loaded.
    pub(crate) server_pinned_threads: HashSet<String>,
    /// Server-known archived ("Other") `thread_id`s preserved even before their conversation is loaded.
    pub(crate) server_archived_threads: HashSet<String>,
    /// How many of the newest messages the chat builds (grows by 60 on
    /// "Load older messages"). Building all 200+ bubbles every frame lagged.
    pub(crate) msg_window: usize,
    /// Names / avatars / member counts (`profiles.rs`, cached a day).
    pub(crate) profiles: crate::profiles::Profiles,
    /// Decoded avatars keyed like [`crate::profiles::Profiles::map`].
    pub(crate) avatars: HashMap<String, Arc<RenderImage>>,
    /// Profile keys whose avatar URL did not decode. Left out of the next
    /// chunk so one bad photo does not keep the visible rows as letters.
    avatar_miss: HashSet<String>,
    avatar_miss_retried: bool,
    pub(crate) profiles_refresh: bool,
    pub(crate) avatars_queued: bool,
    /// One `getprofiles/v2` pass for senders that still have no photo.
    profile_gaps_started: bool,
    /// Group ids already queried via `fetch_missing_groups` this run (prevents
    /// infinite re-fetch loops on left/unavailable groups).
    group_gaps_tried: HashSet<String>,
    /// User ids already queried via `fetch_missing` (`getprofiles/v2`) this run
    /// so opening another conversation can still fill its missing senders without
    /// re-fetching the same unresolvable ids.
    profile_uids_tried: HashSet<String>,
    /// Daily profile fetch is in flight. Gap fill waits so it is not wiped.
    profiles_fetching: bool,
    /// Also load every friend's avatar (group picker open).
    pub(crate) avatar_all: bool,
    /// Add Friend / Create Group / Add Members.
    pub(crate) dialog: Option<crate::dialogs::Dialog>,
    /// Full-size photo viewer.
    pub(crate) viewer: Option<Viewer>,
    /// "Leave Group" needs a second click.
    pub(crate) leave_armed: Option<u64>,
    /// Armed leave uses `/api/group/leave` `silent: 1`.
    pub(crate) leave_silent: bool,
    /// Join-request counts for the info column, keyed by conversation id.
    pub(crate) group_pending_n: HashMap<u64, usize>,
    /// Conversations whose pending-member fetch is in flight.
    pub(crate) group_pending_busy: HashSet<u64>,
    /// Conversations shown in their own window ("Open in Separate Window").
    pub(crate) detached: HashSet<u64>,
    /// This window shows one conversation only (thread id).
    pub(crate) solo: Option<String>,
    /// `zpw_ws` listener started for this window.
    rt_started: bool,
    /// Last realtime state line (None when online).
    pub(crate) rt_status: Option<String>,
    /// Read/seen sync state (`seenv2` outbound, `clearUnreads`/`seens` inbound).
    pub(crate) read_sync: crate::read_sync::ReadSync,
    /// Local id of the outgoing bubble whose seen/received list is open.
    pub(crate) receipt_open: Option<u64>,
    /// Active peer typing indicators by `thread_id -> (sender_uid, last_seen_at)`.
    pub(crate) typing: HashMap<String, (String, std::time::Instant)>,
    /// Last outbound typing ping `(conv_id, at)` to throttle at 5s.
    last_typing_sent: Option<(u64, std::time::Instant)>,
    /// Per-message reactions keyed by local `ChatMsg.id`.
    pub(crate) reactions: HashMap<u64, Vec<crate::read_sync::MsgReaction>>,
    /// Recently sent local reactions `(msg_id, r_type, sent_at)` used to skip
    /// our own WS `612` echo so multi-tap counts never double-increment.
    pub(crate) recent_my_reacts: Vec<(u64, i64, std::time::Instant)>,
    /// Active Like / reaction firework bursts animating in the overlay.
    pub(crate) react_bursts: Vec<crate::context_menu::ReactBurst>,
    /// True while the 60 FPS firework burst timer loop is running.
    pub(crate) react_burst_anim: bool,
    /// Sticker/GIF store state, suggestions, inline commands, and decoded thumbs.
    pub(crate) stickers: StickerState,
    /// Search input inside Sticker tab.
    pub(crate) sticker_search: Entity<LineInput>,
    /// Search input inside GIF tab.
    pub(crate) gif_search: Entity<LineInput>,
    /// Active microphone voice recording handle.
    pub(crate) voice_rec: Option<crate::voice::VoiceRecorder>,
    /// Server-synced Privacy settings (`/api/setting/me`, `/api/setting/update`, block list).
    pub(crate) privacy: crate::settings::PrivacyState,
    /// XDG GlobalShortcuts portal handle + bound status.
    pub(crate) shortcuts: crate::shortcuts::ShortcutState,
    /// Local Xuất / Nhập zip (`pc_export.rs`, independent of Sync 2 and PC→Phone Backup).
    pub(crate) export_ui: crate::pc_export::TransferUi,
    /// Phone → PC Message Sync (Sync 2: `590`/`591`/`592` + `libzproto_wasm_bg`).
    pub(crate) sync_ui: crate::sync_worker::SyncUiState,
    /// PC → Phone Message Backup (`630`–`634` + `/api/backuppc2mb/upload` + `libzproto_backup_wasm_bg`).
    pub(crate) backup_ui: crate::backup_worker::BackupUiState,
    /// Quick-reply popup open above the composer (`cmp-qr`).
    pub(crate) quick_menu: bool,
    /// Composer formatting + urgency strip open (`cmp-fmt`).
    pub(crate) format_open: bool,
    /// Pending message urgency (`0` normal, `1` important, `2` urgent).
    pub(crate) urgency: u8,
    /// Sent/received message urgency tags keyed by local `ChatMsg.id`.
    pub(crate) urgency_msgs: HashMap<u64, u8>,
    /// Floating 6-emoji reaction picker bar (`👍 ❤️ 😆 😮 😢 😠`).
    pub(crate) react_picker: Option<crate::context_menu::ReactPicker>,
    /// Message id whose default reaction button is currently pressed (press-and-hold).
    pub(crate) react_hold_msg: Option<u64>,
    /// True once the press-and-hold timer opened `react_picker` for the current press.
    pub(crate) react_hold_opened: bool,
    /// Generation counter cancelling stale press-and-hold timers.
    pub(crate) react_hold_gen: u64,
    /// Active press-and-hold on the quick-like composer button (`(started_at, press_pos)`).
    pub(crate) like_hold: Option<(std::time::Instant, gpui::Point<gpui::Pixels>)>,
    /// Sent big-like scale tier (`2` = medium 54 px, `3` = large 74 px) keyed by `ChatMsg.id`.
    pub(crate) big_like_sizes: HashMap<u64, u8>,
    /// Cached last window title pushed via `window.set_window_title` (avoids per-frame X11 IPC).
    last_window_title: String,
    /// Cache key for `warm_links`/`warm_polls`/`warm_info_media` so scroll frames skip rescanning messages.
    last_warm_key: Option<(u64, usize, usize, bool, Option<String>)>,
    /// Friend-online dots and the open chat's last-visit line.
    pub(crate) presence: crate::presence::Presence,
    /// Info-column media store preview for the open thread.
    pub(crate) media_shelf: crate::media_store::Shelf,
    /// Per-thread auto-delete TTL in milliseconds from `/api/conv/autodelete/getConvers`.
    pub(crate) auto_delete_ttl: HashMap<String, i64>,
    /// True while a background `load_snapshot_from_dir` reload is running.
    reload_store_inflight: bool,
    /// Queued follow-up reload (`Some(full_refresh)`) coalesced while `reload_store_inflight`.
    reload_store_pending: Option<bool>,
    /// Group threads currently fetching older history via `/api/cm/rgetv2`.
    pub(crate) cloud_history_busy: HashSet<String>,
    /// Group threads with no older cloud history left on the server.
    pub(crate) cloud_history_exhausted: HashSet<String>,
    /// Cached `/api/message/scanurl` safety verdicts keyed by normalized lowercase URL.
    pub(crate) scan_url_cache: HashMap<String, bool>,
    /// Threads currently querying per-member delivered/seen cursors (`getStatusMsg` / WS 575/576).
    pub(crate) status_fetch_busy: HashSet<String>,
    /// Threads whose message status has been fetched at least once this session.
    pub(crate) status_fetched: HashSet<String>,
    /// Threads with Block Sharing enabled (`/api/conv/blocksharing/getConvers`).
    pub(crate) block_sharing: HashSet<String>,
    /// Threads currently toggling Block Sharing.
    pub(crate) block_sharing_busy: HashSet<String>,
    /// Server-synced 1-on-1 pinned topics (`/api/friendboard/list`): `thread_id -> (pins, version)`.
    pub(crate) friend_pins: HashMap<String, (Vec<zalo_client::PinTopicEntry>, i64)>,
    /// 1-on-1 threads whose server pins have been loaded this session.
    pub(crate) friend_pins_fetched: HashSet<String>,
}

/// Newest messages built per open conversation (see `msg_window`).
pub const MSG_WINDOW: usize = 60;

/// Where settings are saved; `None` in unit tests.
fn prefs_dir() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    crate::data::config_dir()
}

/// Full-size photo viewer state.
pub struct Viewer {
    /// Photo bubble.
    pub msg: ChatMsg,
    /// Large decode (None while loading / missing).
    pub image: Option<Arc<RenderImage>>,
    /// 1.0 = fit inside the window. Values above that magnify.
    pub zoom: f32,
    /// Pan from the centred position, in logical pixels. Used once zoomed.
    pub pan_x: f32,
    /// Pan from the centred position, in logical pixels.
    pub pan_y: f32,
    /// Quarter-turns clockwise. 0 keeps the file's orientation.
    pub turns: u8,
    /// OCR recognition in flight inside the viewer.
    pub ocr_busy: bool,
    /// Latest OCR text or localized error message.
    pub ocr_result: Option<Result<String, String>>,
}

impl Viewer {
    pub(crate) fn new(msg: ChatMsg) -> Self {
        Self {
            msg,
            image: None,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            turns: 0,
            ocr_busy: false,
            ocr_result: None,
        }
    }
}

/// "Labels" filter of the conversation list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListFilter {
    /// Every conversation of the tab.
    #[default]
    All,
    /// Only unread.
    Unread,
    /// Folded non-friend 1-1 conversations ("Messages from strangers").
    Strangers,
    /// One label id.
    Label(i64),
}

/// Conversation labels fetch state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LabelsState {
    /// Not requested yet.
    #[default]
    Idle,
    /// Request in flight.
    Loading,
    /// Loaded (possibly empty).
    Ready,
    /// Failed or unavailable (preview / no session).
    Failed(String),
}

/// First conversation, or the one named by `ZALO_GPUI_THREAD` (thread id or
/// title) — a dev hook for screenshots, since Wayland has no click injection.
fn initial_thread(convs: &[ChatConv]) -> Option<u64> {
    let wanted = std::env::var("ZALO_GPUI_THREAD").unwrap_or_default();
    convs
        .iter()
        .find(|row| !wanted.is_empty() && (row.thread_id == wanted || row.name == wanted))
        .or_else(|| convs.first())
        .map(|row| row.id)
}

/// Disk state preloaded *before* GPUI maps the X11 shell window so startup
/// does not block the UI thread while the newly mapped window is black.
pub(crate) struct ShellStartupData {
    snap: data::Snapshot,
    profiles: crate::profiles::Profiles,
    prefs: crate::prefs::Prefs,
    link_previews: HashMap<String, crate::links::LinkPreview>,
}

impl ShellStartupData {
    pub(crate) fn load() -> Self {
        let snap = data::load_snapshot();
        let profile_path = if cfg!(test) {
            None
        } else {
            snap.session_dir
                .as_ref()
                .map(|d| d.join("profiles.json"))
                .or_else(crate::profiles::cache_path)
        };
        let account_prefs_dir = if cfg!(test) {
            None
        } else {
            snap.session_dir.clone()
        };
        Self {
            profiles: crate::profiles::Profiles::load(profile_path.as_deref()),
            prefs: crate::prefs::Prefs::load_scoped(prefs_dir(), account_prefs_dir),
            link_previews: crate::links::load(crate::links::cache_path().as_deref()),
            snap,
        }
    }
}

impl ShellView {
    /// Load `store.db` / `session.json` when present. Tests use preview fixtures.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_data(window, cx, ShellStartupData::load())
    }

    /// Construct the shell using preloaded startup data so no blocking disk I/O
    /// runs after GPUI has already mapped the window.
    pub(crate) fn new_with_data(
        window: &mut Window,
        cx: &mut Context<Self>,
        startup: ShellStartupData,
    ) -> Self {
        window.set_window_title("Zalo");
        cx.observe_window_appearance(window, |_, window, _| {
            window.refresh();
        })
        .detach();
        let search =
            cx.new(|cx| LineInput::new(cx, str_vi("STR_PLACE_HOLDER_SEARCH_PRODUCT", "Tìm kiếm")));
        let composer =
            cx.new(|cx| LineInput::new_multiline(cx, str_vi("STR_SEND_MESSAGE", "Nhập tin nhắn")));
        let caption_input = cx.new(|cx| {
            LineInput::new(
                cx,
                str_vi("STR_INPUT_PREVIEW_EDIT_PHOTO_CAPTION", "Chỉnh sửa mô tả ảnh"),
            )
        });
        let contacts_query = cx.new(|cx| LineInput::new(cx, "Tìm bạn"));
        let msg_search = cx.new(|cx| LineInput::new(cx, "Nhập từ khóa để tìm kiếm"));
        cx.subscribe(&msg_search, |this, _, event: &LineEvent, cx| match event {
            LineEvent::Change(q) => {
                this.update_msg_search(q.clone(), cx);
            }
            LineEvent::Submit(_) => {
                this.step_msg_search(1, cx);
            }
            _ => {}
        })
        .detach();
        let sticker_search = cx.new(|cx| LineInput::new(cx, "Tìm sticker…"));
        let gif_search = cx.new(|cx| LineInput::new(cx, "Tìm GIF…"));
        cx.subscribe(&sticker_search, |this, _, event: &LineEvent, cx| {
            if let LineEvent::Change(text) = event {
                this.on_sticker_search(text, cx);
            }
        })
        .detach();
        cx.subscribe(&gif_search, |this, _, event: &LineEvent, cx| {
            if let LineEvent::Change(text) = event {
                this.on_gif_search(text, cx);
            }
        })
        .detach();
        cx.subscribe(&contacts_query, |this, _, event: &LineEvent, cx| {
            if let LineEvent::Change(text) = event {
                this.contacts_q = text.clone();
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(&search, |this, _, event: &LineEvent, cx| {
            if let LineEvent::Change(text) = event {
                this.search_query = text.clone();
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(
            &composer,
            |this, input, event: &LineEvent, cx| match event {
                LineEvent::Submit(text) => {
                    if this.apply_active_mention(cx) {
                        cx.notify();
                        return;
                    }
                    let text = text.clone();
                    this.composer_link = None;
                    this.composer_link_hide = None;
                    this.send_draft(&text, cx);
                    input.update(cx, |input, cx| input.clear(cx));
                }
                LineEvent::PasteImage(image) => {
                    this.paste_image(image);
                    cx.notify();
                }
                LineEvent::PasteWithChoice { image, text } => {
                    match this.prefs.str("excelPaste", "ask").as_str() {
                        "image" => this.paste_image(image),
                        "text" => {
                            let t = text.clone();
                            input.update(cx, |i, cx| {
                                let mut cur = i.text();
                                cur.push_str(&t);
                                i.set_text(cur, cx);
                            });
                        }
                        _ => this.open_paste_choice(image.clone(), text.clone()),
                    }
                    cx.notify();
                }
                LineEvent::Change(text) => {
                    this.mention_idx = 0;
                    this.on_composer_media_change(text, cx);
                    this.sync_composer_link(text);
                    if !text.is_empty() {
                        this.maybe_send_typing();
                    }
                    cx.notify();
                }
            },
        )
        .detach();
        cx.observe(&composer, |_, _, cx| cx.notify()).detach();
        let ShellStartupData {
            snap,
            profiles,
            prefs,
            link_previews,
        } = startup;
        let mut this = Self {
            rail: Rail::Chats,
            list_filter: ListFilter::All,
            other_tab: false,
            labels: Vec::new(),
            labels_state: LabelsState::Idle,
            labels_pending: false,
            conv_labels: HashMap::new(),
            starred: HashSet::new(),
            starred_open: false,
            pinned_msgs: HashSet::new(),
            selected: initial_thread(&snap.convs),
            contact_tab: ContactTab::Friends,
            note: None,
            note_at: None,
            pending_notice: None,
            notice_thread: None,
            wall_image: RefCell::new(None),
            input_has_sel: false,
            members_view: None,
            select_msgs: None,
            chat_sel: crate::chat_select::ChatTextSelection::default(),
            chat_sel_reg: crate::chat_select::ChatSelectionRegistry::new_shared(),
            link_previews,
            link_pending: HashSet::new(),
            link_thumbs: HashMap::new(),
            composer_link: None,
            composer_link_hide: None,
            poll_pending: HashSet::new(),
            poll_failed: HashSet::new(),
            voice_playing: None,
            contacts_query,
            contacts_q: String::new(),
            contacts_desc: false,
            contacts_by_name: false,
            contacts_label: None,
            recent_phone_searches: Vec::new(),
            requests: None,
            invites: None,
            msg_search,
            msg_search_open: false,
            msg_search_q: String::new(),
            msg_search_hits: Vec::new(),
            msg_search_idx: 0,
            msg_search_sender: None,
            msg_search_sender_label: String::new(),
            msg_search_date_from: None,
            msg_search_date_to: None,
            msg_search_popup: None,
            msg_search_draft_from: None,
            msg_search_draft_to: None,
            msg_search_draft_slot: 0,
            msg_search_cal_ym: {
                let t = crate::calendar::CalDate::today();
                (t.year, t.month)
            },
            msg_search_presets_open: false,
            hidden_srv: crate::privacy::Hidden::default(),
            locked: false,
            lock_error: false,
            lock_input: cx.new(|cx| LineInput::new_masked(cx, "Mã khóa")),
            badge_sent: None,
            root_focus: cx.focus_handle(),
            settings_open: false,
            settings_page: SettingsPage::General,
            sticker_open: false,
            sticker_tab: StickerTab::Sticker,
            english: false,
            show_all_friends: true,
            menu: None,
            pinned: HashSet::new(),
            muted: HashSet::new(),
            archived: HashSet::new(),
            hidden: HashSet::new(),
            removed: HashSet::new(),
            sent_friend_reqs: HashSet::new(),
            marked_read: HashSet::new(),
            reply_to: None,
            convs: snap.convs,
            messages: snap.messages,
            friends: snap.friends,
            groups: snap.groups,
            search,
            composer,
            search_query: String::new(),
            info_open: false,
            info_pref: true,
            info_wide: None,
            compact_level: CompactLevel::Normal,
            compact_show_chat: true,
            chat_pane_w: (1200.0 - zalo_design::SIDEBAR_W).max(320.0),
            info_overlay: false,
            info_join: HashMap::new(),
            info_join_busy: HashSet::new(),
            info_shelf: None,
            shot_with_zalo: true,
            viewer_drag: None,
            viewer_dragged: false,
            forward_of: None,
            display_name: snap.display_name,
            uin: snap.uin,
            user_id: snap.user_id,
            send2me_id: snap.send2me_id,
            session_dir: snap.session_dir,
            has_session: snap.has_session,
            kick_to_login: false,
            left_for_login: false,
            next_msg_id: snap.next_msg_id,
            prefs,
            window_saved: (0, 0, false),
            settings_cache: None,
            pending_photos: Vec::new(),
            pending_thumbs: Vec::new(),
            pending_captions: Vec::new(),
            caption_edit: None,
            caption_input,
            capture_busy: false,
            rail_popup: None,
            update_info: None,
            update_checking: false,
            update_checked_note: None,
            update_install: crate::updater::UpdateInstallState::Idle,
            update_popup_shown_tag: None,
            e2ee_groups: None,
            e2ee_busy: false,
            e2ee_failed: false,
            catalog_rows: Vec::new(),
            catalog_note: String::new(),
            catalog_busy: false,
            catalog_loaded: false,
            media_bitmaps: HashMap::new(),
            media_pending: HashSet::new(),
            media_failed: HashSet::new(),
            media_queue: Vec::new(),
            conv_scroll: gpui::UniformListScrollHandle::new(),
            msg_list: gpui::ListState::new(0, gpui::ListAlignment::Bottom, px(300.)),
            msg_list_state: std::cell::Cell::new((None, 0, 0)),
            chat_scrolled_up: false,
            chat_unread_below: 0,
            new_below_ids: Vec::new(),
            new_mention_below_ids: Vec::new(),
            unread_anchor: None,
            msg_row_ids: std::cell::RefCell::new(Vec::new()),
            unread_divider_row: std::cell::Cell::new(None),
            last_visible_range: std::cell::Cell::new((0, 0)),
            highlight_msg: None,
            last_auto_load_top: None,
            pinned_expanded: false,
            react_detail: None,
            partial_quote_fab: None,
            mention_idx: 0,
            server_pinned_threads: HashSet::new(),
            server_archived_threads: HashSet::new(),
            msg_window: MSG_WINDOW,
            profiles,
            avatars: HashMap::new(),
            avatar_miss: HashSet::new(),
            avatar_miss_retried: false,
            profiles_refresh: false,
            avatars_queued: true,
            profile_gaps_started: false,
            group_gaps_tried: HashSet::new(),
            profile_uids_tried: HashSet::new(),
            profiles_fetching: false,
            avatar_all: false,
            dialog: None,
            viewer: None,
            leave_armed: None,
            leave_silent: false,
            group_pending_n: HashMap::new(),
            group_pending_busy: HashSet::new(),
            detached: HashSet::new(),
            solo: None,
            rt_started: false,
            rt_status: None,
            read_sync: crate::read_sync::ReadSync::default(),
            receipt_open: None,
            typing: HashMap::new(),
            last_typing_sent: None,
            reactions: HashMap::new(),
            recent_my_reacts: Vec::new(),
            react_bursts: Vec::new(),
            react_burst_anim: false,
            stickers: StickerState {
                recent: crate::stickers::load_recent_stickers(),
                ..StickerState::default()
            },
            sticker_search,
            gif_search,
            voice_rec: None,
            privacy: Default::default(),
            shortcuts: Default::default(),
            export_ui: Default::default(),
            sync_ui: Default::default(),
            backup_ui: Default::default(),
            quick_menu: false,
            format_open: false,
            urgency: 0,
            urgency_msgs: HashMap::new(),
            react_picker: None,
            react_hold_msg: None,
            react_hold_opened: false,
            react_hold_gen: 0,
            like_hold: None,
            big_like_sizes: HashMap::new(),
            last_window_title: String::new(),
            last_warm_key: None,
            presence: crate::presence::Presence::default(),
            media_shelf: crate::media_store::Shelf::default(),
            auto_delete_ttl: HashMap::new(),
            reload_store_inflight: false,
            reload_store_pending: None,
            cloud_history_busy: HashSet::new(),
            cloud_history_exhausted: HashSet::new(),
            scan_url_cache: HashMap::new(),
            status_fetch_busy: HashSet::new(),
            status_fetched: HashSet::new(),
            block_sharing: HashSet::new(),
            block_sharing_busy: HashSet::new(),
            friend_pins: HashMap::new(),
            friend_pins_fetched: HashSet::new(),
        };
        if let Some(saved) = this.prefs.json("receipts") {
            this.read_sync.load_cursors(&saved);
        }
        if let Some(id) = this.selected {
            this.warm_media(id);
            this.scroll_msgs_to_bottom();
        }
        // Older caches have no `friend` flag: refresh once so Contacts is real.
        // A group list with members but no `creatorId` cannot put the owner
        // first in the collage, and caches without `global_id` cannot resolve
        // Sync 2 conversations, so refresh once through the same fetch.
        let flagged = this.profiles.map.values().any(|p| p.friend);
        let needs_creator = this
            .profiles
            .map
            .values()
            .any(|p| !p.member_ids.is_empty() && p.creator_id.is_empty());
        let needs_global_id = !this.profiles.map.is_empty()
            && !this.profiles.map.values().any(|p| !p.global_id.is_empty());
        this.profiles_refresh = this.has_session
            && (this.profiles.stale(data::now_ms())
                || !flagged
                || needs_creator
                || needs_global_id);
        this.wire_lock_input(cx);
        // A set screen-lock code also locks at start-up (zalo-gui behaviour).
        if !this.prefs.str("lockHash", "").is_empty() {
            this.locked = true;
            let handle = gpui::Focusable::focus_handle(this.lock_input.read(cx), cx);
            window.focus(&handle);
        }
        this.load_hidden(cx);
        this.load_auto_delete(cx);
        this.load_server_conv_states(cx);
        if let Ok(mode) = std::env::var("ZALO_GPUI_CALL") {
            this.demo_call(&mode, cx);
        }
        this.load_conv_flags();
        this.apply_saved_read();
        let data_dir = this.prefs.str("dataDir", "");
        if !data_dir.is_empty() {
            crate::media::set_media_root(Some(data_dir.into()));
        }
        this.show_all_friends = this.prefs.str("friendFilter", "all") != "zalo";
        this.info_pref = this.prefs.bool("showRightSidebar", true);
        this.shot_with_zalo = this.prefs.bool("shotWithZalo", true);
        this.english = match std::env::var("ZALO_GPUI_LANG").as_deref() {
            Ok(l) => l == "en",
            Err(_) => this.prefs.str("lang", "vi") == "en",
        };
        crate::i18n::set_english(this.english);
        this.apply_theme(window);
        {
            let weak_scroll = cx.weak_entity();
            this.msg_list.set_scroll_handler(move |ev, _window, cx| {
                let visible = ev.visible_range.clone();
                let count = ev.count;
                let _ = weak_scroll.update(cx, |view, cx| {
                    let mut dirty = view.handle_msg_scroll(visible.clone(), count);
                    if view.maybe_auto_load_top(visible.start, count, cx) {
                        dirty = true;
                    }
                    if dirty {
                        cx.notify();
                    }
                });
            });
        }
        if !cfg!(test) {
            window.activate_window();
            let weak = cx.weak_entity();
            window
                .spawn(cx, async move |cx| {
                    // X11/XFCE (`xfwm4`) strips server decorations asynchronously after
                    // `_MOTIF_WM_HINTS` is applied on the first frame; nudge repaints so
                    // the window paints immediately without waiting for mouse hover.
                    for delay_ms in [50u64, 180, 400] {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(delay_ms))
                            .await;
                        let _ = weak.update_in(cx, |_, window, cx| {
                            window.refresh();
                            cx.notify();
                        });
                    }
                })
                .detach();
        }
        if let Ok(page) = std::env::var("ZALO_GPUI_SETTINGS") {
            if let Some(p) =
                crate::settings::SettingsPage::ALL.get(page.parse::<usize>().unwrap_or(0))
            {
                this.settings_open = true;
                this.settings_page = *p;
                this.refresh_settings_cache(*p);
            }
        }
        if let Ok(tab) = std::env::var("ZALO_GPUI_STICKER") {
            this.sticker_open = true;
            this.sticker_tab = match tab.as_str() {
                "emoji" => crate::stickers::StickerTab::Emoji,
                "gif" => crate::stickers::StickerTab::Gif,
                _ => crate::stickers::StickerTab::Sticker,
            };
            this.ensure_sticker_data(cx);
        }
        this.open_dev_menu(window);
        this.rail_popup = match std::env::var("ZALO_GPUI_POPUP").as_deref() {
            Ok("zcloud") => Some(crate::rail::RailPopup::ZCloud),
            Ok("tools") => Some(crate::rail::RailPopup::Tools),
            Ok("starred") => {
                this.starred_open = true;
                None
            }
            Ok("catalog") => Some(crate::rail::RailPopup::Catalog),
            _ => None,
        };
        if std::env::var("ZALO_GPUI_CONTACTS").as_deref() == Ok("groups") {
            this.rail = crate::rail::Rail::Contacts;
            this.contact_tab = ContactTab::Groups;
        }
        if std::env::var_os("ZALO_GPUI_INFO").is_some() {
            this.info_open = true;
            this.info_pref = true;
        }
        match std::env::var("ZALO_GPUI_DIALOG").as_deref() {
            Ok("friend") => this.open_add_friend(cx),
            Ok("group") => this.open_group_picker(None, cx),
            Ok("profile") => this.open_profile_dialog(cx),
            Ok("quick") => this.open_quick_messages(cx),
            Ok("board") => {
                if let Some(id) = this.selected {
                    this.open_board_dialog(id, crate::board::BoardTab::All, cx);
                }
            }
            Ok("manage") => {
                if let Some(id) = this.selected {
                    this.open_group_manage(id, cx);
                }
            }
            Ok("autodelete") => {
                if let Some(id) = this.selected {
                    this.open_auto_delete(id, cx);
                }
            }
            Ok("resetpin") => {
                this.open_pin(crate::privacy::PinMode::ResetHidden, cx);
            }
            Ok("viewer") => {
                let photo = this.selected.and_then(|id| {
                    this.messages_of(id)
                        .iter()
                        .rev()
                        .find(|m| m.kind == crate::media::MsgKind::Photo)
                        .cloned()
                });
                if let Some(m) = photo {
                    this.open_viewer(m, window, cx);
                }
            }
            _ => {}
        }
        this.apply_profile_aliases();
        if !cfg!(test) && this.solo.is_none() && this.prefs.bool("checkUpdates", true) {
            this.start_update_poller(cx);
        }
        this
    }

    fn start_update_poller(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            gpui::Timer::after(std::time::Duration::from_secs(5)).await;
            loop {
                let alive = this
                    .update(cx, |view, cx| {
                        view.trigger_update_check(false, cx);
                    })
                    .is_ok();
                if !alive {
                    break;
                }
                gpui::Timer::after(std::time::Duration::from_secs(
                    crate::updater::UPDATE_POLL_INTERVAL_SECS,
                ))
                .await;
            }
        })
        .detach();
    }

    /// Queries GitHub Releases asynchronously for a version newer than `APP_VERSION`.
    pub(crate) fn trigger_update_check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.update_checking {
            return;
        }
        self.update_checking = true;
        if manual {
            self.update_checked_note = None;
        }
        let etag = if manual {
            None
        } else {
            let saved = self.prefs.str("updateCheckEtag", "");
            if saved.is_empty() {
                None
            } else {
                Some(saved)
            }
        };
        cx.notify();

        let (tx, rx) = futures::channel::oneshot::channel();
        crate::ws_rpc::async_rt().spawn(async move {
            let res = crate::updater::check_latest_release(crate::APP_VERSION, etag.as_deref()).await;
            let _ = tx.send(res);
        });

        cx.spawn(async move |this, cx| {
            let Ok((result, next_etag)) = rx.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                view.update_checking = false;
                if let Some(tag) = next_etag {
                    view.prefs
                        .set("updateCheckEtag", serde_json::Value::String(tag));
                }
                match result {
                    crate::updater::UpdateCheckResult::Available(info) => {
                        let already_notified =
                            view.prefs.str("notifiedUpdateVersion", "") == info.tag;
                        if !already_notified {
                            view.prefs.set(
                                "notifiedUpdateVersion",
                                serde_json::Value::String(info.tag.clone()),
                            );
                            crate::updater::notify_update_available(&info);
                        }
                        let tag = info.tag.clone();
                        let dismissed =
                            view.prefs.str("dismissedUpdateVersion", "") == tag;
                        let already_popped =
                            view.update_popup_shown_tag.as_deref() == Some(tag.as_str());
                        view.update_info = Some(info);
                        view.update_checked_note = None;
                        if manual {
                            view.update_popup_shown_tag = Some(tag);
                            view.dialog = Some(crate::dialogs::Dialog::UpdateAvailable);
                        } else if view.solo.is_none()
                            && view.dialog.is_none()
                            && !dismissed
                            && !already_popped
                        {
                            view.update_popup_shown_tag = Some(tag);
                            view.dialog = Some(crate::dialogs::Dialog::UpdateAvailable);
                        }
                    }
                    crate::updater::UpdateCheckResult::UpToDate => {
                        view.update_info = None;
                        if manual {
                            view.update_checked_note =
                                Some(crate::i18n::t("Đang dùng phiên bản mới nhất"));
                        }
                    }
                    crate::updater::UpdateCheckResult::NotModified => {
                        if manual && view.update_info.is_none() {
                            view.update_checked_note =
                                Some(crate::i18n::t("Đang dùng phiên bản mới nhất"));
                        }
                    }
                    crate::updater::UpdateCheckResult::Failed(err) => {
                        if manual {
                            view.update_checked_note = Some(format!(
                                "{} ({err})",
                                crate::i18n::t("Không kiểm tra được cập nhật")
                            ));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Dismisses automatic popup prompts for the currently advertised release tag.
    pub(crate) fn dismiss_update_version(&mut self, cx: &mut Context<Self>) {
        if self.update_install.is_busy() {
            return;
        }
        if let Some(tag) = self.update_info.as_ref().map(|u| u.tag.clone()) {
            self.prefs.set(
                "dismissedUpdateVersion",
                serde_json::Value::String(tag.clone()),
            );
            self.update_popup_shown_tag = Some(tag);
        }
        if matches!(self.dialog, Some(crate::dialogs::Dialog::UpdateAvailable)) {
            self.dialog = None;
        }
        cx.notify();
    }

    /// Downloads, verifies SHA-256, installs, and restarts into the detected release.
    pub(crate) fn start_update_install(&mut self, cx: &mut Context<Self>) {
        if self.update_install.is_busy() {
            return;
        }
        let Some(info) = self.update_info.clone() else {
            return;
        };
        if !info.package_kind.supports_in_app_install() {
            cx.open_url(&info.release_url);
            self.set_note(crate::i18n::t("Đã mở trang tải bản cập nhật trên trình duyệt"));
            cx.notify();
            return;
        }

        self.dialog = Some(crate::dialogs::Dialog::UpdateAvailable);
        self.update_install = crate::updater::UpdateInstallState::Downloading {
            downloaded: 0,
            total: info.asset_size,
        };
        cx.notify();

        let release_url = info.release_url.clone();
        let (progress_tx, mut progress_rx) =
            futures::channel::mpsc::unbounded::<crate::updater::UpdateInstallState>();
        let (done_tx, done_rx) =
            futures::channel::oneshot::channel::<Result<crate::updater::ApplyOutcome, String>>();

        crate::ws_rpc::async_rt().spawn(async move {
            let outcome = crate::updater::download_and_apply_update(info, move |st| {
                let _ = progress_tx.unbounded_send(st);
            })
            .await;
            let _ = done_tx.send(outcome);
        });

        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(st) = progress_rx.next().await {
                if this
                    .update(cx, |view, cx| {
                        view.update_install = st;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            let Ok(outcome) = done_rx.await else {
                return;
            };
            match outcome {
                Ok(crate::updater::ApplyOutcome::QuitToRestart) => {
                    gpui::Timer::after(std::time::Duration::from_millis(450)).await;
                    let _ = cx.update(|cx| cx.quit());
                }
                Ok(crate::updater::ApplyOutcome::OpenReleasePage) => {
                    let _ = this.update(cx, |view, cx| {
                        view.update_install = crate::updater::UpdateInstallState::Idle;
                        cx.open_url(&release_url);
                        cx.notify();
                    });
                }
                Err(err) => {
                    let _ = this.update(cx, |view, cx| {
                        view.update_install = crate::updater::UpdateInstallState::Failed(err);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// Queue every photo / sticker / GIF of one thread that has no bitmap
    /// yet. Decoding (JXL included) and CDN fetch run in the background from
    /// `render`; the UI thread never decodes. CDN only for live threads with a
    /// saved session dir — preview fixtures never touch the network.
    pub(crate) fn warm_media(&mut self, conv_id: u64) {
        let auto_dl = self.prefs.bool("autoDownload", true);
        let live = self.session_dir.is_some()
            && self.convs.iter().any(|row| row.id == conv_id && row.live);
        let max_auto = 30 * 1024 * 1024_u64;
        let jobs: Vec<crate::fetch::Job> = self
            .messages_of(conv_id)
            .iter()
            .filter(|msg| {
                let remote_ok = live
                    && auto_dl
                    && crate::media::is_http_url(&msg.media_url)
                    && (msg.file_size == 0 || msg.file_size <= max_auto);
                matches!(
                    msg.kind,
                    crate::media::MsgKind::Photo
                        | crate::media::MsgKind::Doodle
                        | crate::media::MsgKind::Sticker
                        | crate::media::MsgKind::Gif
                        | crate::media::MsgKind::Video
                        | crate::media::MsgKind::Card
                ) && (msg.media_path.is_some() || remote_ok)
                    && !self.media_bitmaps.contains_key(&msg.id)
                    && !self.media_pending.contains(&msg.id)
            })
            .map(|msg| crate::fetch::Job {
                msg_id: msg.id,
                path: msg.media_path.clone(),
                url: if live && auto_dl {
                    msg.media_url.clone()
                } else {
                    String::new()
                },
                max_edge: match msg.kind {
                    crate::media::MsgKind::Sticker => crate::media::STICKER_SIZE as u32,
                    crate::media::MsgKind::Card => 96,
                    _ => 320,
                },
            })
            .collect();
        let jobs = crate::fetch::newest_n(jobs, self.msg_window.max(crate::fetch::MAX_PER_THREAD));
        for job in &jobs {
            self.media_pending.insert(job.msg_id);
        }
        self.media_queue.extend(jobs);
    }

    /// Queue thumbnails for the info panel's "Photos/Videos" grid (the newest
    /// eight photos/videos, which may be older than the loaded message window).
    pub(crate) fn warm_info_media(&mut self, conv_id: u64) {
        let live = self.session_dir.is_some()
            && self.convs.iter().any(|row| row.id == conv_id && row.live);
        let jobs: Vec<crate::fetch::Job> = self
            .messages_of(conv_id)
            .iter()
            .rev()
            .filter(|m| {
                matches!(
                    m.kind,
                    crate::media::MsgKind::Photo
                        | crate::media::MsgKind::Doodle
                        | crate::media::MsgKind::Gif
                        | crate::media::MsgKind::Video
                )
            })
            .take(8)
            .filter(|m| {
                let has_url = m.media_url.starts_with("https://")
                    || m.media_url.starts_with("http://");
                (m.media_path.is_some() || (live && has_url))
                    && !self.media_bitmaps.contains_key(&m.id)
                    && !self.media_pending.contains(&m.id)
            })
            .map(|m| crate::fetch::Job {
                msg_id: m.id,
                path: m.media_path.clone(),
                url: if live {
                    m.media_url.clone()
                } else {
                    String::new()
                },
                max_edge: crate::media::PHOTO_MAX_W as u32,
            })
            .collect();
        for job in &jobs {
            self.media_pending.insert(job.msg_id);
        }
        self.media_queue.extend(jobs);
    }

    /// Unread total (muted chats excluded) on the tray icon and launcher.
    fn publish_badge(&mut self) {
        if self.solo.is_some() || cfg!(test) {
            return;
        }
        let total: i64 = self
            .convs
            .iter()
            .filter(|c| !self.muted.contains(&c.id))
            .map(|c| i64::from(self.unread_of(c)))
            .sum();
        if self.badge_sent != Some(total) {
            self.badge_sent = Some(total);
            zalo_shell::set_unread_badge(total);
        }
    }

    /// Link cards of the open conversation: ask `parselink` once per URL
    /// and download preview thumbnails.
    fn warm_links(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let mut urls: Vec<String> = self
            .messages_of(id)
            .iter()
            .rev()
            .take(self.msg_window.max(40))
            .filter_map(|m| {
                if m.kind == crate::media::MsgKind::Link && m.href.starts_with("http") {
                    Some(m.href.clone())
                } else if m.kind == crate::media::MsgKind::Text {
                    // A URL inside a sentence stays text; still ask for a preview.
                    crate::media::first_link(&m.text).filter(|url| m.text.trim() != url)
                } else {
                    None
                }
            })
            .collect();
        if let Some(url) = self.composer_link.clone() {
            if !urls.iter().any(|u| u == &url) {
                urls.push(url);
            }
        }
        let dir = self.session_dir.clone().filter(|_| self.has_session);
        let mut ask = Vec::new();
        let mut thumbs = Vec::new();
        for url in urls {
            match self.link_previews.get(&url) {
                None if dir.is_some() && self.link_pending.insert(url.clone()) => ask.push(url),
                Some(p) if !p.thumb.is_empty() && !self.link_thumbs.contains_key(&p.thumb) => {
                    if self.link_pending.insert(p.thumb.clone()) {
                        thumbs.push(p.thumb.clone());
                    }
                }
                _ => {}
            }
        }
        if let (Some(dir), false) = (dir.clone(), ask.is_empty()) {
            cx.spawn(async move |this, cx| {
                let got = cx
                    .background_executor()
                    .spawn(async move {
                        crate::bridge::block_api(&dir, |api| async move {
                            let mut out = Vec::new();
                            for url in ask {
                                // Failures stay out of the cache (retried next launch).
                                match api.parse_link(&url).await {
                                    Ok(v) => {
                                        if std::env::var_os("ZALO_GPUI_DEBUG_LINKS").is_some() {
                                            eprintln!("zalo-gpui parselink {url}: {v}");
                                        }
                                        out.push((url, crate::links::from_parselink(&v)));
                                    }
                                    Err(e) => eprintln!("zalo-gpui parselink {url}: {e}"),
                                }
                            }
                            Ok(out)
                        })
                    })
                    .await;
                let _ = this.update(cx, |view, cx| {
                    if let Ok(list) = got {
                        for (url, p) in list {
                            view.link_previews.insert(url, p);
                        }
                        if let Some(path) = crate::links::cache_path() {
                            crate::links::save(&path, &view.link_previews);
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        if !thumbs.is_empty() {
            let jobs: Vec<crate::fetch::Job> = thumbs
                .iter()
                .enumerate()
                .map(|(i, url)| crate::fetch::Job {
                    msg_id: i as u64,
                    path: None,
                    url: url.clone(),
                    max_edge: 600,
                })
                .collect();
            cx.spawn(async move |this, cx| {
                let done = cx
                    .background_executor()
                    .spawn(async move { crate::fetch::run_batch(dir.as_deref(), jobs) })
                    .await;
                let _ = this.update(cx, |view, cx| {
                    for d in done {
                        if let (Some(img), Some(url)) = (d.image, thumbs.get(d.msg_id as usize)) {
                            view.link_thumbs.insert(url.clone(), img);
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Composer `.chat-input-link-preview`: the first complete URL in the draft.
    ///
    /// Requires a host with at least a 2-letter alphabetic TLD (e.g. `.vn`, `.com`,
    /// `.lol`) or a path/port so typing partial domains like `https://lza.l` does
    /// not fire invalid `parselink` requests (`error 114`).
    fn sync_composer_link(&mut self, text: &str) {
        let url = crate::media::first_link(text).filter(|u| {
            let host = crate::media::host_of(u);
            let bare = host.split(':').next().unwrap_or(host);
            let Some((_, tld)) = bare.rsplit_once('.') else {
                return false;
            };
            let tld_ok = tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic());
            // Only preview once the URL is followed by whitespace/path/query or
            // has a TLD of >= 3 chars (or common 2-char ccTLDs like `.vn`/`.io`),
            // avoiding transient partial-TLD fetches while typing `.lo` -> `.lol`.
            let after_url = text
                .find(u.as_str())
                .map(|i| i + u.len() < text.len())
                .unwrap_or(false);
            let has_path = u
                .strip_prefix("https://")
                .or_else(|| u.strip_prefix("http://"))
                .is_some_and(|rest| rest.contains('/'));
            let common_2 = matches!(
                tld.to_ascii_lowercase().as_str(),
                "vn" | "io" | "ai" | "me" | "co" | "tv" | "us" | "uk" | "jp" | "kr" | "de" | "fr"
            );
            bare.len() > 3 && tld_ok && (after_url || has_path || tld.len() >= 3 || common_2)
        });
        if self.composer_link_hide.is_some() && self.composer_link_hide != url {
            self.composer_link_hide = None;
        }
        if self.composer_link_hide.is_some() && self.composer_link_hide == url {
            self.composer_link = None;
            return;
        }
        self.composer_link = url;
    }

    /// `group.poll` bubbles that only carry `pollId` load `/api/poll/detail`.
    fn warm_polls(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            return;
        };
        let ids: Vec<i64> = self
            .messages_of(id)
            .iter()
            .rev()
            .take(self.msg_window.max(40))
            .filter(|m| {
                m.kind == crate::media::MsgKind::Poll && m.poll_id > 0 && m.poll_options.is_empty()
            })
            .map(|m| m.poll_id)
            .filter(|id| !self.poll_pending.contains(id) && !self.poll_failed.contains(id))
            .collect::<HashSet<_>>()
            .into_iter()
            .take(6)
            .collect();
        if ids.is_empty() {
            return;
        }
        for id in &ids {
            self.poll_pending.insert(*id);
        }
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let mut out = Vec::new();
                        for id in ids {
                            match api.get_poll_detail(id).await {
                                Ok(v) => out.push((id, Some(v))),
                                Err(e) => {
                                    eprintln!("zalo-gpui poll detail {id}: {e}");
                                    out.push((id, None));
                                }
                            }
                        }
                        Ok(out)
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let list = got.unwrap_or_default();
                for (id, value) in list {
                    view.poll_pending.remove(&id);
                    match value
                        .as_ref()
                        .and_then(|v| crate::media::poll_from_value(v))
                    {
                        Some(body) if body.id == 0 || body.id == id => {
                            let mut body = body;
                            if body.id == 0 {
                                body.id = id;
                            }
                            view.apply_poll_body(&body);
                        }
                        _ => {
                            view.poll_failed.insert(id);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_poll_body(&mut self, body: &crate::media::PollBody) {
        for msgs in self.messages.values_mut() {
            for msg in msgs.iter_mut() {
                if msg.kind != crate::media::MsgKind::Poll || msg.poll_id != body.id {
                    continue;
                }
                if !body.question.is_empty() {
                    msg.file_name = body.question.clone();
                    if msg.text.starts_with("[Bình chọn]") {
                        msg.text = format!("[Bình chọn] {}", body.question);
                    }
                }
                if !body.options.is_empty() {
                    msg.poll_options = body.options.clone();
                }
                msg.poll_closed = body.closed;
            }
        }
    }

    /// Vote one option, then refresh the card from `/api/poll/detail`.
    pub(crate) fn vote_on_poll(&mut self, poll_id: i64, option_id: i64, cx: &mut Context<Self>) {
        let Some(dir) = self.session_dir.clone().filter(|_| self.has_session) else {
            self.set_note("Chưa đăng nhập");
            return;
        };
        if poll_id <= 0 || option_id <= 0 {
            return;
        }
        for msgs in self.messages.values_mut() {
            for msg in msgs.iter_mut() {
                if msg.poll_id != poll_id {
                    continue;
                }
                for opt in &mut msg.poll_options {
                    if opt.id == option_id && !opt.voted {
                        opt.voted = true;
                        opt.votes += 1;
                    }
                }
            }
        }
        self.set_note("Đã gửi bình chọn");
        self.poll_failed.remove(&poll_id);
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.vote_poll(poll_id, &[option_id])
                            .await
                            .map_err(|e| e.to_string())?;
                        api.get_poll_detail(poll_id)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match got {
                    Ok(value) => {
                        if let Some(mut body) = crate::media::poll_from_value(&value) {
                            if body.id == 0 {
                                body.id = poll_id;
                            }
                            view.apply_poll_body(&body);
                        }
                        view.note = None;
                    }
                    Err(err) => view.set_note(format!("Bình chọn thất bại: {err}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Voice bubble ▶ / ■: download once to the media cache, play in-process
    /// via `symphonia` + `CardSpeaker` (AMR goes through Zalo's `/mp3?url=` like Zalo PC).
    pub(crate) fn toggle_voice(&mut self, msg: &ChatMsg, cx: &mut Context<Self>) {
        if let Some((id, handle)) = self.voice_playing.take() {
            handle.stop();
            if id == msg.id {
                cx.notify();
                return;
            }
        }
        let local = msg.media_path.clone().filter(|p| p.is_file());
        let (url, path) = if let Some(p) = local {
            (String::new(), p)
        } else {
            let url = zalo_client::voice_play_href(&msg.href, msg.global_msg_id);
            let Some(path) = crate::media::cache_path_for(&url) else {
                self.set_note("Tin nhắn thoại không có đường dẫn tải");
                return;
            };
            (url, path)
        };
        let dir = self.session_dir.clone().filter(|_| self.has_session);
        let id = msg.id;
        if !path.is_file() {
            self.set_note("Đang tải tin nhắn thoại…");
        }
        cx.spawn(async move |this, cx| {
            let ready = cx
                .background_executor()
                .spawn(async move {
                    if !path.is_file() {
                        let dir = dir.ok_or_else(|| "chưa đăng nhập".to_owned())?;
                        let bytes = crate::bridge::block_api(&dir, |api| async move {
                            api.fetch_limited(&url, 20 << 20)
                                .await
                                .map_err(|e| e.to_string())
                        })?;
                        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
                    }
                    crate::audio_play::play_file(path, None, false, None)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match ready {
                    Ok(handle) => {
                        view.voice_playing = Some((id, handle));
                        view.note = None;
                        // Clear the ■ state once in-process playback finishes.
                        cx.spawn(async move |this, cx| loop {
                            gpui::Timer::after(std::time::Duration::from_millis(250)).await;
                            let done = this
                                .update(cx, |view, cx| {
                                    let finished = match view.voice_playing.as_ref() {
                                        Some((pid, handle)) if *pid == id => handle.is_finished(),
                                        _ => true,
                                    };
                                    if finished {
                                        if view
                                            .voice_playing
                                            .as_ref()
                                            .is_some_and(|(pid, _)| *pid == id)
                                        {
                                            view.voice_playing = None;
                                        }
                                        cx.notify();
                                    }
                                    finished
                                })
                                .unwrap_or(true);
                            if done {
                                break;
                            }
                        })
                        .detach();
                    }
                    Err(e) => view.set_note(format!("Không phát được: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Video bubble opens `Zalo Photo` with the filmstrip, same as a photo.
    /// Play inside that window uses the local file once it is on disk.
    pub(crate) fn open_video(&mut self, msg: &ChatMsg, cx: &mut Context<Self>) {
        let local = std::path::PathBuf::from(&msg.href);
        if local.is_file() {
            let mut shown = msg.clone();
            shown.media_path = Some(local);
            self.note = None;
            self.open_photo(shown, false, cx);
            return;
        }
        if !msg.href.starts_with("https://") {
            self.set_note("Video không có đường dẫn tải");
            return;
        }
        let url = msg.href.clone();
        let Some(path) = crate::media::cache_path_for(&url).map(|p| p.with_extension("mp4")) else {
            return;
        };
        let dir = self.session_dir.clone().filter(|_| self.has_session);
        let shown_msg = msg.clone();
        self.set_note("Đang tải video…");
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    if !path.is_file() {
                        let dir = dir.ok_or_else(|| "chưa đăng nhập".to_owned())?;
                        let bytes = crate::bridge::block_api(&dir, |api| async move {
                            api.fetch_limited(&url, 200 << 20)
                                .await
                                .map_err(|e| e.to_string())
                        })?;
                        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
                    }
                    Ok::<_, String>(path)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match got {
                    Ok(path) => {
                        view.note = None;
                        let mut shown = shown_msg;
                        shown.href = path.display().to_string();
                        shown.media_path = Some(path);
                        view.open_photo(shown, false, cx);
                    }
                    Err(e) => view.set_note(format!("Không tải được video: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn persist_window_state(&mut self, window: &Window) {
        if cfg!(test) {
            return;
        }
        let b = window.window_bounds().get_bounds();
        let w = f32::from(b.size.width).round() as i64;
        let h = f32::from(b.size.height).round() as i64;
        let max = window.is_maximized();
        if (w, h, max) == self.window_saved {
            return;
        }
        if w < 400 || h < 300 {
            return;
        }
        self.window_saved = (w, h, max);
        self.prefs
            .set("window", serde_json::json!({ "w": w, "h": h, "max": max }));
    }

    /// Removes `session.json` when `rememberLogin` is disabled cleanly on logout or final quit.
    pub(crate) fn clear_session_if_unremembered(&self) {
        if self.prefs.bool("rememberLogin", true) {
            return;
        }
        if let Some(dir) = self.session_dir.clone().or_else(crate::data::config_dir) {
            crate::data::wipe_session_file(&dir);
        }
    }

    /// Wipe the saved PC session, optionally tell the server, then open QR login.
    ///
    /// `server_logout` fires `/api/v2/login/logOut` best-effort. Relogin after
    /// 102/600 skips it so the QR window is the only `request_qr`.
    pub(crate) fn leave_to_login(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
        server_logout: bool,
    ) {
        if self.left_for_login {
            return;
        }
        self.left_for_login = true;
        self.kick_to_login = false;
        self.sync_ui.reset();
        self.backup_ui.reset();
        // Stop the shared realtime hub & WebSocket immediately so the logged-out
        // account cannot emit notifications and a subsequent login starts clean.
        crate::background::stop();
        let dir = self.session_dir.clone().or_else(crate::data::config_dir);
        let logout_snapshot = if server_logout && self.has_session {
            dir.as_ref().and_then(|d| {
                let device = zalo_client::Device::load_or_create(d).ok()?;
                let session = zalo_client::StoredSession::load(d).ok()?;
                Some((device, session, d.clone()))
            })
        } else {
            None
        };
        if let Some(d) = dir {
            // Drop the bearer session and the current-user pointer. The account
            // directory (chats of this userId) stays. Another account lives in
            // its own directory and is not opened until its own QR succeeds.
            crate::data::detach_account(&d);
        }
        if let Some((device, session, d)) = logout_snapshot {
            crate::bridge::spawn_logout_snapshot(device, session, d);
        } else {
            crate::bridge::clear_login_cache();
        }
        self.has_session = false;
        self.dialog = None;
        self.menu = None;
        self.selected = None;
        self.convs.clear();
        self.messages.clear();
        zalo_shell::set_unread_badge(0);
        // Open login window first so the app never hits zero windows on Wayland,
        // then close this currently borrowed shell window directly and defer-close
        // any remaining solo/auxiliary windows (PhotoWindow, CallView, Notice, etc.).
        crate::open_login_window(cx);
        window.remove_window();
        cx.defer(crate::close_all_shell_windows);
    }

    /// Start concurrent background fetches for everything in [`Self::media_queue`].
    /// Each photo streams to the UI as soon as IT finishes (newest-first, bounded
    /// concurrency), so one slow or expired URL never holds up a newly received photo.
    pub(crate) fn drain_media_queue(&mut self, cx: &mut Context<Self>) {
        if self.media_queue.is_empty() {
            return;
        }
        let dir = self.session_dir.clone();
        let now = data::now_ms();
        let mut jobs: Vec<(crate::fetch::Job, bool, Option<crate::fetch::RenewHint>)> =
            std::mem::take(&mut self.media_queue)
                .into_iter()
                .map(|job| {
                    let meta = self.messages.values().find_map(|list| {
                        list.iter()
                            .find(|m| m.id == job.msg_id)
                            .map(|m| (m.at_ms, m.thread_id.clone(), m.kind))
                    });
                    let recent = meta
                        .as_ref()
                        .is_some_and(|(at, _, _)| *at == 0 || now.saturating_sub(*at) < 5 * 60_000);
                    let renew = meta.and_then(|(_, tid, kind)| {
                        if !self.has_session || !data::live_allowed(&tid) {
                            return None;
                        }
                        let msg_type = match kind {
                            crate::media::MsgKind::Photo | crate::media::MsgKind::Doodle => {
                                zalo_client::MSGTYPE_FORWARD_PHOTO
                            }
                            crate::media::MsgKind::Video => zalo_client::MSGTYPE_FORWARD_VIDEO,
                            _ => return None,
                        };
                        Some(crate::fetch::RenewHint {
                            is_group: tid.starts_with("g_"),
                            thread_id: tid,
                            msg_type,
                        })
                    });
                    (job, recent, renew)
                })
                .collect();
        // Newest first so newly received / bottom photos appear immediately.
        jobs.reverse();
        let bg = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            let mut stream = futures::stream::iter(jobs)
                .map(move |(job, recent, renew)| {
                    let dir = dir.clone();
                    bg.spawn(async move {
                        crate::fetch::run_one_with_renew(dir.as_deref(), job, recent, renew)
                    })
                })
                .buffer_unordered(crate::fetch::BUDGET);
            while let Some(done) = stream.next().await {
                let alive = this.update(cx, |view, cx| {
                    view.apply_media(vec![done]);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Scrolls `#message-view` to the bottom bubble.
    pub(crate) fn scroll_msgs_to_bottom(&self) {
        let count = self.msg_list.item_count();
        self.msg_list.scroll_to(gpui::ListOffset {
            item_ix: count,
            offset_in_item: px(0.),
        });
    }

    /// True when `#message-view` is pinned at (or near) the bottom.
    pub(crate) fn is_msgs_at_bottom(&self) -> bool {
        let count = self.msg_list.item_count();
        count == 0 || self.msg_list.logical_scroll_top().item_ix >= count.saturating_sub(1)
    }

    fn apply_media(&mut self, done: Vec<crate::fetch::Done>) {
        let at_bottom = self.is_msgs_at_bottom();
        for item in done {
            match item.image {
                Some(image) => {
                    self.media_failed.remove(&item.msg_id);
                    self.media_bitmaps.insert(item.msg_id, image);
                }
                None => {
                    self.media_failed.insert(item.msg_id);
                }
            }
            if item.path.is_some() || item.renewed_url.is_some() {
                for msgs in self.messages.values_mut() {
                    if let Some(msg) = msgs.iter_mut().find(|m| m.id == item.msg_id) {
                        if let Some(path) = item.path.as_ref() {
                            msg.media_path = Some(path.clone());
                        }
                        if let Some(fresh_url) = item.renewed_url.as_ref() {
                            msg.media_url = fresh_url.clone();
                        }
                        break;
                    }
                }
            }
            // Keep failed ids pending so a dead URL is not retried every open.
        }
        if at_bottom {
            self.scroll_msgs_to_bottom();
        }
    }

    /// Toast at the bottom of the window, hidden after ~3.5 s.
    pub(crate) fn set_note(&mut self, text: impl Into<String>) {
        self.note = Some(text.into());
        self.note_at = None;
    }

    /// Arm the auto-hide timer the first time a toast is painted.
    fn arm_note_timer(&mut self, cx: &mut Context<Self>) {
        if self.note.is_none() || self.note_at.is_some() {
            return;
        }
        let at = std::time::Instant::now();
        self.note_at = Some(at);
        cx.spawn(async move |this, cx| {
            gpui::Timer::after(std::time::Duration::from_millis(3500)).await;
            let _ = this.update(cx, |view, cx| {
                if view.note_at == Some(at) {
                    view.note = None;
                    view.note_at = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Queue a screen popup. Newlines become spaces, then the 352×82 card clips.
    pub(crate) fn show_msg_notice(
        &mut self,
        title: String,
        body: String,
        initial: String,
        accent: u32,
        image: Option<Arc<RenderImage>>,
        collage: Vec<crate::sidebar::CollageFace>,
    ) {
        let body = crate::notice::notice_line(&body);
        let body = {
            const N: usize = 140;
            if body.chars().count() <= N {
                body
            } else {
                let mut s: String = body.chars().take(N).collect();
                s.push('…');
                s
            }
        };
        let thread_key = self.notice_thread.clone().unwrap_or_default();
        self.pending_notice = Some(crate::notice::NoticeContent {
            title,
            body,
            initial,
            accent,
            image,
            collage,
            thread_key,
        });
    }

    /// Write a dragged screen position into prefs.
    pub(crate) fn apply_notice_custom(&mut self) {
        let Some((x, y)) = crate::notice::take_custom() else {
            return;
        };
        self.prefs
            .set("notifyCorner", serde_json::Value::String("custom".into()));
        self.prefs.set("notifyX", serde_json::json!(x));
        self.prefs.set("notifyY", serde_json::json!(y));
    }

    fn notice_duration(&self) -> std::time::Duration {
        let secs = self
            .prefs
            .json("notifyDuration")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .unwrap_or(crate::notice::DEFAULT_DURATION_SECS);
        std::time::Duration::from_secs(crate::notice::clamp_duration_secs(secs))
    }

    fn flush_notice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_notice_custom();
        let Some(content) = self.pending_notice.take() else {
            return;
        };
        let corner = self.prefs.str("notifyCorner", "tr");
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
        let duration = self.notice_duration();
        crate::notice::present(
            cx,
            window.scale_factor(),
            content,
            &corner,
            x,
            y,
            duration,
        );
    }

    /// Clear sidebar conversation search input and query, plus any open in-thread
    /// search drawer so switching rail tabs always lands cleanly in the target section.
    pub(crate) fn clear_sidebar_search(&mut self, cx: &mut Context<Self>) {
        self.search_query.clear();
        if !self.search.read(cx).text().is_empty() {
            self.search.update(cx, |input, cx| input.clear(cx));
        }
        self.msg_search_open = false;
        self.msg_search_popup = None;
    }

    pub(crate) fn set_rail(&mut self, rail: Rail) {
        self.search_query.clear();
        self.msg_search_open = false;
        self.msg_search_popup = None;
        if rail == Rail::Settings {
            self.settings_open = true;
            self.refresh_settings_cache(self.settings_page);
            self.sticker_open = false;
            self.menu = None;
            return;
        }
        self.rail = rail;
        self.rail_popup = None;
        self.starred_open = false;
        self.note = None;
        self.sticker_open = false;
        self.menu = None;
        self.forward_of = None;
        self.settings_open = false;
        if rail == Rail::Chats {
            self.list_filter = ListFilter::All;
            self.compact_show_chat = false;
        } else if rail == Rail::Contacts {
            self.compact_show_chat = false;
        } else if rail == Rail::Docs {
            self.ensure_my_documents_conv();
            if let Some(id) = self.cloud_id() {
                self.selected = Some(id);
                self.warm_media(id);
                self.scroll_msgs_to_bottom();
            }
            self.compact_show_chat = true;
        } else {
            self.compact_show_chat = true;
        }
    }

    /// Reload shared `notifyOff` after a tray or Quicklist toggle.
    pub(crate) fn sync_notify_pref_from_disk(&mut self) {
        let fresh = crate::prefs::Prefs::load(crate::data::config_dir());
        let muted = fresh.bool("notifyOff", false);
        self.prefs.set("notifyOff", serde_json::Value::Bool(muted));
    }

    /// Open the in-app tray fallback menu at top-right of the shell chrome.
    pub(crate) fn open_tray_context_menu(&mut self, window: &Window) {
        let pos = gpui::point(
            px((f32::from(window.viewport_size().width) - 246.0).max(80.0)),
            px(zalo_design::LINUX_TITLEBAR_H + 8.0),
        );
        self.open_menu(crate::context_menu::MenuKind::Tray, pos, window);
    }

    /// Dev hook for screenshots: `ZALO_GPUI_MENU=conv|msg|account|filter|more`
    /// opens that menu at start (Wayland has no click injection).
    fn open_dev_menu(&mut self, window: &Window) {
        use crate::context_menu::MenuKind;
        let Ok(which) = std::env::var("ZALO_GPUI_MENU") else {
            return;
        };
        let at = |x: f32, y: f32| gpui::point(px(x), px(y));
        let (kind, pos) = match which.as_str() {
            "conv" => match self.selected {
                Some(id) => (MenuKind::Conv(id), at(250., 250.)),
                None => return,
            },
            "msg" => match self
                .selected
                .and_then(|id| self.messages_of(id).last().cloned())
            {
                Some(msg) => (MenuKind::Message(msg), at(700., 300.)),
                None => return,
            },
            "account" => (MenuKind::Account, at(40., 70.)),
            "filter" => (MenuKind::Filter, at(390., 130.)),
            "more" => (MenuKind::TabMore, at(470., 130.)),
            _ => return,
        };
        self.open_menu(kind, pos, window);
        if let Ok(sub) = std::env::var("ZALO_GPUI_SUBMENU") {
            if let (Some(menu), Ok(i)) = (self.menu.as_mut(), sub.parse::<usize>()) {
                menu.sub = Some(i);
            }
        }
    }

    /// Drop the live socket so the next frame opens the account just imported.
    pub(crate) fn mark_realtime_stale(&mut self) {
        crate::background::stop();
        self.rt_started = false;
    }

    /// Start the `zpw_ws` listener once per window when a session exists.
    fn start_realtime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use futures::StreamExt;
        if self.rt_started || !self.has_session || cfg!(test) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        self.rt_started = true;
        // Bridge outcomes (send failures, file sent) → note bar.
        let (rtx, mut rrx) = futures::channel::mpsc::unbounded::<String>();
        crate::bridge::set_reporter(rtx);
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                while let Some(text) = rrx.next().await {
                    if weak
                        .update(cx, |view, cx| {
                            view.set_note(text);
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        // One process-wide listener (tray mode keeps it while the window is closed).
        crate::background::subscribe(dir, tx);
        let this = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                while let Some(event) = rx.next().await {
                    if this
                        .update_in(cx, |view, window, cx| {
                            match event {
                                crate::realtime::RtEvent::Voip(list) => view.on_voip(list, cx),
                                other => view.on_realtime(other, window, cx),
                            }
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
    }

    fn on_realtime(
        &mut self,
        event: crate::realtime::RtEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::realtime::RtEvent;
        match event {
            RtEvent::Online => {
                self.rt_status = None;
                if !self.avatar_miss.is_empty() {
                    self.avatar_miss.clear();
                    self.avatar_miss_retried = false;
                    self.avatars_queued = true;
                }
                if self.has_session {
                    if let Some(dir) = self.session_dir.clone() {
                        crate::bridge::flush_outbox(dir);
                    }
                }
            }
            RtEvent::Offline(why) => {
                eprintln!("zalo-gpui realtime offline: {why}");
                if crate::login_flow::is_session_error(&why) {
                    // 102, 600, or -69. Do not call server logout: the session
                    // is already dead, and the account directory stays.
                    self.leave_to_login(window, cx, false);
                    return;
                } else if zalo_client::listener::is_transient_disconnect(&why)
                    || why.contains("http://")
                    || why.contains("https://")
                {
                    self.rt_status = Some("Mất kết nối máy chủ tin nhắn, đang kết nối lại…".into());
                } else {
                    let brief: String = why.chars().take(80).collect();
                    self.rt_status = Some(format!("Mất kết nối nhận tin ({brief}), đang thử lại…"));
                }
            }
            RtEvent::Replaced => {
                // Cmd 3000 or close 3000. Electron retries the close, then a
                // real kick is error 600. This client already stopped the
                // socket on 3000, so return to QR instead of staying in chat.
                // Close 3003 never becomes Replaced.
                self.leave_to_login(window, cx, false);
            }
            RtEvent::Voip(_) => {}
            RtEvent::Status(status) => self.apply_status(status),
            RtEvent::Typing(list) => {
                let now = std::time::Instant::now();
                let mine = self.self_member_ids();
                let mut added = false;
                for t in list {
                    if !t.thread_id.is_empty() && !t.user_id.is_empty() && !mine.contains(&t.user_id) {
                        self.typing.insert(t.thread_id, (t.user_id, now));
                        added = true;
                    }
                }
                if added {
                    self.schedule_typing_expiry(cx);
                }
            }
            RtEvent::Reactions(list) => self.apply_reactions(list),
            RtEvent::Pinned(raw_ids) => self.apply_server_pinned(&raw_ids),
            RtEvent::Archived(raw_ids) => self.apply_server_archived(&raw_ids),
            RtEvent::RemoteConfigs { sync, backup } => {
                self.sync_ui.config = sync;
                self.backup_ui.config = backup;
                self.on_backup_services_ready();
            }
            RtEvent::SyncAfterLogin(payload) => {
                self.handle_transfer_after_login(payload, cx);
            }
            RtEvent::Messages(list, live) => {
                let active = window.is_window_active();
                for m in list {
                    self.apply_incoming(m, live, active);
                }
                self.flush_notice(window, cx);
            }
            RtEvent::Undo(undos) => {
                for u in undos {
                    self.apply_incoming_undo(&u);
                }
            }
        }
    }

    /// Applies an incoming `chat.undo` recall from another user or another device,
    /// collapsing the targeted message into `"Tin nhắn đã thu hồi"` and clearing
    /// its media/quote fields so it never lingers beside a resent message.
    pub(crate) fn apply_incoming_undo(&mut self, u: &crate::realtime::UndoEvent) {
        let Some(conv_id) = self
            .convs
            .iter()
            .find(|c| c.thread_id == u.thread_id)
            .map(|c| c.id)
        else {
            return;
        };
        let recalled = str_vi("STR_UNDO_MESSAGE", "Tin nhắn đã thu hồi").to_owned();
        let mut updated_last = false;
        if let Some(list) = self.messages.get_mut(&conv_id) {
            let last_idx = list.len().saturating_sub(1);
            for (idx, row) in list.iter_mut().enumerate().rev() {
                let matched = u
                    .target_global_id
                    .filter(|&g| g > 0)
                    .is_some_and(|g| row.global_msg_id == Some(g))
                    || u.target_cli_id
                        .filter(|&c| c > 0)
                        .is_some_and(|c| row.cli_msg_id == Some(c));
                if matched {
                    row.recalled = true;
                    row.text = recalled.clone();
                    row.kind = crate::media::MsgKind::Text;
                    row.media_url.clear();
                    row.media_path = None;
                    row.href.clear();
                    row.file_name.clear();
                    row.quote_sender.clear();
                    row.quote_text.clear();
                    row.group_layout_id = 0;
                    if idx == last_idx {
                        updated_last = true;
                    }
                    break;
                }
            }
        }
        if updated_last {
            if let Some(c) = self.convs.iter_mut().find(|c| c.id == conv_id) {
                c.preview = recalled;
            }
        }
    }

    /// Insert one message (dedupe by server id, or by client id for our own
    /// echo). A live message bumps unread, moves the thread up, and notifies.
    /// History stays in time order and only moves the thread when it is newest.
    pub(crate) fn apply_incoming(
        &mut self,
        m: crate::realtime::Incoming,
        live: bool,
        active: bool,
    ) {
        let conv_id = match self.convs.iter().position(|c| c.thread_id == m.thread_id) {
            Some(i) => self.convs[i].id,
            None => {
                let is_docs = self
                    .send2me_id
                    .as_deref()
                    .is_some_and(|sid| !sid.is_empty() && sid == m.thread_id);
                let name = if is_docs {
                    "My Documents".to_owned()
                } else {
                    self.profile_ref(&m.thread_id)
                        .map(|p| p.name.clone())
                        .filter(|n| !n.is_empty() && !crate::profiles::is_id_label(&m.thread_id, n))
                        .unwrap_or_else(|| {
                            if m.is_group {
                                "Nhóm".to_owned()
                            } else {
                                m.sender_name.clone()
                            }
                        })
                };
                if m.is_group
                    && name != "Nhóm"
                    && !crate::profiles::is_id_label(&m.thread_id, &name)
                {
                    data::persist_thread_title(&m.thread_id, &name);
                }
                let id = data::hash_id(&m.thread_id);
                self.convs.insert(
                    0,
                    ChatConv {
                        id,
                        thread_id: m.thread_id.clone(),
                        name,
                        preview: String::new(),
                        time: String::new(),
                        unread: 0,
                        is_group: m.is_group,
                        online: false,
                        live: data::live_allowed(&m.thread_id),
                    },
                );
                if self.server_pinned_threads.contains(&m.thread_id) {
                    self.pinned.insert(id);
                }
                if self.server_archived_threads.contains(&m.thread_id) {
                    self.archived.insert(id);
                }
                self.avatars_queued = true;
                // A group with no real server name (or missing both own photo
                // and member list for collage) still needs getmg-v2 / getgi/v2.
                if m.is_group {
                    let ready = self.profile_ref(&m.thread_id).is_some_and(|p| {
                        let label = p.name.trim();
                        let named = !label.is_empty()
                            && label != "Nhóm"
                            && !crate::profiles::is_id_label(&m.thread_id, label);
                        let has_visual =
                            crate::sidebar::is_real_avatar(&p.avatar) || !p.member_ids.is_empty();
                        named && has_visual
                    });
                    if !ready {
                        self.profile_gaps_started = false;
                    }
                }
                id
            }
        };
        let viewing =
            self.selected == Some(conv_id) && active && !self.locked && self.rail == Rail::Chats;
        if !m.from_me {
            self.typing.remove(&m.thread_id);
            if let Some(rec) = &m.seen_record {
                self.read_sync.note_incoming(&m.thread_id, m.at_ms, rec);
                self.send_delivered_if_due(&m.thread_id);
            }
            if viewing && !self.chat_scrolled_up {
                self.send_seen_if_due(&m.thread_id);
            }
        }
        {
            let mut upgraded_media = false;
            let id = self.messages.get_mut(&conv_id).and_then(|list| {
                let row = list.iter_mut().rev().find(|x| {
                    data::is_duplicate_msg(
                        x,
                        m.global_msg_id,
                        m.cli_msg_id,
                        m.from_me,
                        &m.sender_uid,
                        m.kind,
                        &m.text,
                        &m.media_url,
                        &m.href,
                        m.at_ms,
                    )
                })?;
                if row.global_msg_id.unwrap_or(0) <= 0 && m.global_msg_id.unwrap_or(0) > 0 {
                    row.global_msg_id = m.global_msg_id;
                }
                if row.cli_msg_id.unwrap_or(0) <= 0 && m.cli_msg_id.unwrap_or(0) > 0 {
                    row.cli_msg_id = m.cli_msg_id;
                }
                upgraded_media = merge_incoming_into_row(row, &m);
                if row.quote_text.is_empty() && !m.quote_text.is_empty() {
                    row.quote_sender = m.quote_sender.clone();
                    row.quote_text = m.quote_text.clone();
                    row.quote_global_id = m.quote_global_id;
                    row.quote_cli_id = m.quote_cli_id;
                }
                if m.group_layout_id != 0 {
                    row.group_layout_id = m.group_layout_id;
                }
                if matches!(
                    m.kind,
                    crate::media::MsgKind::Call | crate::media::MsgKind::MissedCall
                ) {
                    row.call_video |= m.call_video;
                    if row.call_reason == 0 && m.call_reason != 0 {
                        row.call_reason = m.call_reason;
                    }
                    if row.file_size == 0 && m.duration_ms > 0 {
                        row.file_size = m.duration_ms;
                    }
                    if row.file_name.is_empty() && !m.file_name.is_empty() {
                        row.file_name = m.file_name.clone();
                    }
                }
                Some(row.id)
            });
            if let Some(id) = id {
                if m.kind == crate::media::MsgKind::Link {
                    let link_key = if !m.href.is_empty() {
                        m.href.clone()
                    } else {
                        crate::media::first_link(&m.text).unwrap_or_default()
                    };
                    if !link_key.is_empty()
                        && (!m.link_title.is_empty() || !m.media_url.is_empty())
                    {
                        self.link_previews.insert(
                            link_key.clone(),
                            crate::links::LinkPreview {
                                title: m.link_title.clone(),
                                desc: m.link_desc.clone(),
                                thumb: m.media_url.clone(),
                                src: crate::media::host_of(&link_key).to_owned(),
                            },
                        );
                        if let Some(path) = crate::links::cache_path() {
                            crate::links::save(&path, &self.link_previews);
                        }
                    }
                }
                if upgraded_media {
                    self.media_bitmaps.remove(&id);
                    self.media_pending.remove(&id);
                    self.media_failed.remove(&id);
                    if self.selected == Some(conv_id) {
                        self.warm_media(conv_id);
                        self.warm_info_media(conv_id);
                    }
                }
                if m.urgency > 0 {
                    self.urgency_msgs.insert(id, m.urgency);
                }
                return;
            }
        }
        let list = self.messages.entry(conv_id).or_default();
        if m.from_me {
            // Echo of what this window just sent: same clientId, or (media
            // without a matching id) same kind within two minutes.
            let same_cli = |x: &ChatMsg| x.cli_msg_id.is_some() && x.cli_msg_id == m.cli_msg_id;
            let same_media = |x: &ChatMsg| {
                x.kind == m.kind
                    && matches!(
                        m.kind,
                        crate::media::MsgKind::Photo
                            | crate::media::MsgKind::Doodle
                            | crate::media::MsgKind::File
                            | crate::media::MsgKind::Sticker
                            | crate::media::MsgKind::Gif
                            | crate::media::MsgKind::Voice
                            | crate::media::MsgKind::Video
                    )
                    && (x.at_ms - m.at_ms).abs() < 120_000
            };
            if let Some(own) = list
                .iter_mut()
                .find(|x| x.from_me && x.global_msg_id.is_none() && (same_cli(x) || same_media(x)))
            {
                own.global_msg_id = m.global_msg_id;
                own.cli_msg_id = m.cli_msg_id.or(own.cli_msg_id);
                if own.media_url.is_empty() {
                    own.media_url = m.media_url.clone();
                }
                if own.href.is_empty() {
                    own.href = m.href.clone();
                }
                if m.group_layout_id != 0 {
                    own.group_layout_id = m.group_layout_id;
                }
                if own.file_size == 0 {
                    own.file_size = m.file_size;
                }
                if own.quote_text.is_empty() && !m.quote_text.is_empty() {
                    own.quote_sender = m.quote_sender.clone();
                    own.quote_text = m.quote_text.clone();
                    own.quote_global_id = m.quote_global_id;
                    own.quote_cli_id = m.quote_cli_id;
                }
                if m.urgency > 0 {
                    self.urgency_msgs.insert(own.id, m.urgency);
                }
                return;
            }
        }
        let id = if m.row_id > 0 {
            m.row_id as u64
        } else {
            self.next_msg_id
        };
        self.next_msg_id = self.next_msg_id.max(id + 1);
        let sender = if m.from_me {
            str_vi("STR_YOU", "Bạn").to_owned()
        } else {
            let known = self
                .profiles
                .map
                .get(&m.sender_uid)
                .map(|p| p.name.as_str())
                .unwrap_or("");
            crate::media::sender_label(
                &m.sender_uid,
                if known.is_empty() {
                    &m.sender_name
                } else {
                    known
                },
            )
        };
        let mut msg = ChatMsg::text_msg(
            id,
            conv_id,
            m.thread_id.clone(),
            m.from_me,
            sender.clone(),
            m.sender_uid.clone(),
            m.text.clone(),
            data::vn_hm(m.at_ms),
            m.at_ms,
            m.cli_msg_id,
            m.global_msg_id,
        );
        msg.kind = m.kind;
        msg.group_layout_id = m.group_layout_id;
        msg.media_url = m.media_url.clone();
        msg.media_w = m.width;
        msg.media_h = m.height;
        msg.file_name = m.file_name.clone();
        msg.file_size = m.file_size;
        msg.call_video = m.call_video;
        msg.call_reason = m.call_reason;
        if matches!(
            m.kind,
            crate::media::MsgKind::Call | crate::media::MsgKind::MissedCall
        ) && m.duration_ms > 0
        {
            msg.file_size = m.duration_ms;
        }
        if !m.quote_sender.is_empty() || !m.quote_text.is_empty() {
            msg.quote_sender = if m.quote_sender == "Bạn" || m.quote_sender.is_empty() {
                m.quote_sender.clone()
            } else {
                self.profiles
                    .map
                    .get(&m.quote_sender)
                    .map(|p| p.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| crate::media::sender_label(&m.quote_sender, &m.quote_sender))
            };
            msg.quote_text = m.quote_text.clone();
            msg.quote_global_id = m.quote_global_id;
            msg.quote_cli_id = m.quote_cli_id;
        }
        if m.kind == crate::media::MsgKind::Link {
            let link_href = if !m.href.is_empty() {
                m.href.clone()
            } else {
                crate::media::first_link(&m.text).unwrap_or_else(|| m.text.clone())
            };
            msg.href = link_href.clone();
            if !link_href.is_empty() && (!m.link_title.is_empty() || !m.media_url.is_empty()) {
                self.link_previews.insert(
                    link_href.clone(),
                    crate::links::LinkPreview {
                        title: m.link_title.clone(),
                        desc: m.link_desc.clone(),
                        thumb: m.media_url.clone(),
                        src: crate::media::host_of(&link_href).to_owned(),
                    },
                );
                msg.media_url = String::new();
                if let Some(path) = crate::links::cache_path() {
                    crate::links::save(&path, &self.link_previews);
                }
            }
        }
        if matches!(
            m.kind,
            crate::media::MsgKind::Voice | crate::media::MsgKind::Video
        ) {
            msg.href = m.href.clone();
            msg.file_size = m.duration_ms;
        }
        if matches!(
            m.kind,
            crate::media::MsgKind::Location | crate::media::MsgKind::Ecard
        ) {
            msg.href = m.href.clone();
        }
        if m.urgency > 0 {
            self.urgency_msgs.insert(msg.id, m.urgency);
        }
        if m.kind == crate::media::MsgKind::Poll {
            msg.poll_id = m.poll_id;
            msg.file_name = if m.poll_question.is_empty() {
                m.file_name.clone()
            } else {
                m.poll_question.clone()
            };
            msg.poll_action = m.poll_action.clone();
            msg.poll_closed = m.poll_closed;
            msg.poll_options = m.poll_options.clone();
        }
        let inserted_msg_id = msg.id;
        let inserted_text = msg.text.clone();
        let list = self.messages.entry(conv_id).or_default();
        let pos = list
            .iter()
            .position(|x| x.at_ms > m.at_ms && x.at_ms > 0)
            .unwrap_or(list.len());
        list.insert(pos, msg);
        let newest = pos + 1 == list.len();
        let viewing =
            self.selected == Some(conv_id) && active && !self.locked && self.rail == Rail::Chats;
        let mentions_me = live
            && !m.from_me
            && message_mentions_me(&inserted_text, self.display_name.as_deref());
        if let Some(i) = self.convs.iter().position(|c| c.id == conv_id) {
            let mut conv = self.convs.remove(i);
            if live && !m.from_me {
                if viewing {
                    if self.chat_scrolled_up {
                        if !self.new_below_ids.contains(&inserted_msg_id) {
                            self.new_below_ids.push(inserted_msg_id);
                        }
                        if mentions_me && !self.new_mention_below_ids.contains(&inserted_msg_id) {
                            self.new_mention_below_ids.push(inserted_msg_id);
                        }
                        self.chat_unread_below = self.new_below_ids.len() as u32;
                        let above = self.unread_above_count(conv_id);
                        conv.unread = above.saturating_add(self.chat_unread_below);
                        self.marked_read.remove(&conv_id);
                    } else {
                        let above = self.unread_above_count(conv_id);
                        if above > 0 {
                            conv.unread = above;
                        } else {
                            conv.unread = 0;
                            self.marked_read.insert(conv_id);
                            self.mark_thread_read_pref(&m.thread_id, m.at_ms);
                        }
                    }
                } else {
                    conv.unread += 1;
                    self.marked_read.remove(&conv_id);
                }
            }
            // An older offline page must not reorder the sidebar or rewrite its clock.
            if live || newest {
                conv.time = data::vn_hm(m.at_ms);
                self.convs.insert(0, conv.clone());
            } else {
                self.convs.insert(i, conv.clone());
            }
            if live
                && !m.from_me
                && !viewing
                && !self.prefs.bool("notifyOff", false)
                && !self.muted.contains(&conv_id)
            {
                if self.prefs.bool("notifySound", true) {
                    crate::background::play_notify_sound();
                }
                let body = if self.prefs.bool("peekGuard", false) {
                    "Tin nhắn mới".to_owned()
                } else if conv.is_group {
                    format!("{sender}: {}", m.text)
                } else {
                    m.text.clone()
                };
                let title = if conv.is_group {
                    let prof_name = self
                        .profile_ref(&conv.thread_id)
                        .map(|p| p.name.as_str())
                        .unwrap_or("");
                    crate::profiles::group_display_name(&conv.thread_id, &conv.name, prof_name)
                } else {
                    conv.name.clone()
                };
                let initial = {
                    let init = crate::sidebar::avatar_initials(&title);
                    if init.is_empty() {
                        "Z".into()
                    } else {
                        init
                    }
                };
                let accent = crate::sidebar::avatar_color(&conv.thread_id);
                self.notice_thread = Some(conv.thread_id.clone());
                let image = self.shown_photo(&conv.thread_id);
                let collage = if conv.is_group && image.is_none() {
                    self.group_collage_faces(&conv.thread_id)
                } else {
                    Vec::new()
                };
                if image.is_none() {
                    self.avatar_miss.remove(&conv.thread_id);
                    self.avatar_miss.remove(&alt_profile_key(&conv.thread_id));
                    if conv.is_group {
                        for uid in self.group_collage_uids(&conv.thread_id) {
                            self.avatar_miss.remove(&uid);
                        }
                    }
                    self.avatars_queued = true;
                }
                if conv.is_group
                    && (title == "Nhóm"
                        || crate::profiles::is_id_label(&conv.thread_id, &title)
                        || (image.is_none() && collage.is_empty()))
                {
                    self.group_gaps_tried.remove(&conv.thread_id);
                    self.profile_gaps_started = false;
                }
                self.show_msg_notice(title, body, initial, accent, image, collage);
            }
        }
        if self.selected == Some(conv_id) {
            self.warm_media(conv_id);
            if (live || newest) && (!self.chat_scrolled_up || m.from_me) {
                if m.from_me {
                    self.clear_open_unread_and_mark_read(conv_id);
                }
                self.scroll_msgs_to_bottom();
            }
        }
    }

    /// Pinned / muted / Other live in `ui-state.json` (same keys and thread
    /// ids as zalo-gui); hidden / removed in `gpui-prefs.json`.
    fn load_conv_flags(&mut self) {
        let ids = |key: &str| -> Vec<String> {
            serde_json::from_str::<Vec<String>>(&self.prefs.raw(key)).unwrap_or_default()
        };
        let pinned_list = ids("pinned");
        let archived_list = ids("other");
        self.server_pinned_threads = pinned_list.iter().cloned().collect();
        self.server_archived_threads = archived_list.iter().cloned().collect();
        let map: std::collections::HashMap<String, u64> = self
            .convs
            .iter()
            .map(|c| (c.thread_id.clone(), c.id))
            .collect();
        let to_ids = |list: Vec<String>| -> HashSet<u64> {
            list.iter().filter_map(|t| map.get(t).copied()).collect()
        };
        self.pinned = to_ids(pinned_list);
        self.muted = to_ids(ids("muted"));
        self.archived = to_ids(archived_list);
        self.hidden = to_ids(ids("hidden"));
        self.removed = to_ids(ids("removed"));
        self.starred = parse_starred_ids(&self.prefs.raw("starredMsgs"));
    }

    /// Persist the per-conversation flags (see [`Self::load_conv_flags`]).
    pub(crate) fn save_conv_flags(&mut self) {
        let loaded_live: HashSet<String> = self
            .convs
            .iter()
            .filter(|c| data::live_allowed(&c.thread_id))
            .map(|c| c.thread_id.clone())
            .collect();
        let threads = |set: &HashSet<u64>, extra: Option<&HashSet<String>>| -> serde_json::Value {
            let mut v: Vec<String> = self
                .convs
                .iter()
                .filter(|c| set.contains(&c.id) && data::live_allowed(&c.thread_id))
                .map(|c| c.thread_id.clone())
                .collect();
            if let Some(extra_set) = extra {
                for tid in extra_set {
                    if !loaded_live.contains(tid) && data::live_allowed(tid) {
                        v.push(tid.clone());
                    }
                }
            }
            v.sort();
            v.dedup();
            serde_json::json!(v)
        };
        // Keep server_pinned_threads / server_archived_threads consistent with manual toggles on loaded convs.
        for c in &self.convs {
            if !data::live_allowed(&c.thread_id) {
                continue;
            }
            if self.pinned.contains(&c.id) {
                self.server_pinned_threads.insert(c.thread_id.clone());
            } else {
                self.server_pinned_threads.remove(&c.thread_id);
            }
            if self.archived.contains(&c.id) {
                self.server_archived_threads.insert(c.thread_id.clone());
            } else {
                self.server_archived_threads.remove(&c.thread_id);
            }
        }
        let (p, m, o, h, r) = (
            threads(&self.pinned, Some(&self.server_pinned_threads)),
            threads(&self.muted, None),
            threads(&self.archived, Some(&self.server_archived_threads)),
            threads(&self.hidden, None),
            threads(&self.removed, None),
        );
        self.prefs.set("pinned", p);
        self.prefs.set("muted", m);
        self.prefs.set("other", o);
        self.prefs.set("hidden", h);
        self.prefs.set("removed", r);
    }

    /// Persist starred bubbles. Electron stores the same list in IndexedDB
    /// (`StarMessage`); the PC client has no server star API.
    pub(crate) fn save_starred(&mut self) {
        self.prefs
            .set("starredMsgs", starred_ids_json(&self.starred));
    }

    fn apply_saved_read(&mut self) {
        let Ok(map) = serde_json::from_str::<HashMap<String, i64>>(&self.prefs.raw("read")) else {
            return;
        };
        for conv in &mut self.convs {
            if !conv.live || conv.unread == 0 {
                continue;
            }
            let Some(&saved_at) = map.get(&conv.thread_id) else {
                continue;
            };
            let last_at = self
                .messages
                .get(&conv.id)
                .and_then(|l| l.last())
                .map(|m| m.at_ms)
                .unwrap_or(0);
            if saved_at > 0 && (last_at == 0 || last_at <= saved_at) {
                conv.unread = 0;
                self.marked_read.insert(conv.id);
            }
        }
    }

    pub(crate) fn mark_thread_read_pref(&mut self, thread_id: &str, at_ms: i64) {
        if !data::live_allowed(thread_id) || at_ms <= 0 {
            return;
        }
        let mut map = serde_json::from_str::<HashMap<String, i64>>(&self.prefs.raw("read"))
            .unwrap_or_default();
        let entry = map.entry(thread_id.to_owned()).or_insert(0);
        if at_ms > *entry {
            *entry = at_ms;
            self.prefs.set("read", serde_json::json!(map));
        }
    }

    pub(crate) fn apply_status(&mut self, status: zalo_client::StatusUpdate) {
        let messages = &self.messages;
        let convs = &self.convs;
        let me = self.self_uid().map(str::to_owned);
        let cleared = self.read_sync.apply_status(&status, me.as_deref(), |mark| {
            if let Some(ts) = mark.ts.filter(|&t| t > 0) {
                return Some(if ts < 10_000_000_000 { ts * 1000 } else { ts });
            }
            let gid = mark.last_msg_id?;
            let cid = convs.iter().find(|c| c.thread_id == mark.thread_id)?.id;
            let list = messages.get(&cid)?;
            if let Some(found) = list.iter().find(|m| m.global_msg_id == Some(gid)) {
                return (found.at_ms > 0).then_some(found.at_ms);
            }
            let newest = list.iter().filter_map(|m| m.global_msg_id).max();
            if newest.is_some_and(|n| gid >= n) {
                Some(list.last().map(|m| m.at_ms).unwrap_or(0).max(1))
            } else {
                None
            }
        });
        for (thread_id, cutoff) in cleared {
            if let Some(conv) = self.convs.iter_mut().find(|c| c.thread_id == thread_id) {
                let conv_id = conv.id;
                let last_at = self
                    .messages
                    .get(&conv_id)
                    .and_then(|l| l.last())
                    .map(|m| m.at_ms)
                    .unwrap_or(0);
                if last_at == 0 || last_at <= cutoff {
                    conv.unread = 0;
                    self.marked_read.insert(conv_id);
                }
            }
            self.mark_thread_read_pref(&thread_id, cutoff);
        }
        self.prefs.set("receipts", self.read_sync.cursors_json());
    }

    pub(crate) fn apply_reactions(&mut self, list: Vec<crate::realtime::ReactionUpdate>) {
        let now = std::time::Instant::now();
        self.recent_my_reacts
            .retain(|(_, _, at)| now.duration_since(*at) < std::time::Duration::from_secs(4));
        for r in list {
            let mut found_id: Option<(u64, u64)> = None;
            if let Some(conv_id) = self
                .convs
                .iter()
                .find(|c| c.thread_id == r.thread_id)
                .map(|c| c.id)
            {
                if let Some(msgs) = self.messages.get(&conv_id) {
                    if let Some(m) = msgs.iter().find(|m| {
                        r.target_global_id
                            .is_some_and(|g| m.global_msg_id == Some(g))
                            || r.target_cli_id.is_some_and(|c| m.cli_msg_id == Some(c))
                    }) {
                        found_id = Some((conv_id, m.id));
                    }
                }
            }
            // Fallback across all loaded threads when `gMsgID` is present.
            if found_id.is_none() {
                if let Some(gid) = r.target_global_id {
                    'outer: for (&cid, msgs) in &self.messages {
                        for m in msgs {
                            if m.global_msg_id == Some(gid) {
                                found_id = Some((cid, m.id));
                                break 'outer;
                            }
                        }
                    }
                }
            }
            let Some((conv_id, msg_id)) = found_id else {
                continue;
            };
            // Skip immediate WS echo of a reaction we just applied optimistically.
            if r.from_me {
                let echo_type = if r.cleared { -1 } else { r.r_type };
                if let Some(pos) = self
                    .recent_my_reacts
                    .iter()
                    .position(|&(id, rt, _)| id == msg_id && rt == echo_type)
                {
                    self.recent_my_reacts.remove(pos);
                    continue;
                }
            }
            crate::read_sync::update_reaction(
                &mut self.reactions,
                msg_id,
                &r.sender_uid,
                &r.r_icon,
                r.r_type,
                r.cleared,
            );
            let has_any = self.reactions.get(&msg_id).is_some_and(|v| !v.is_empty());
            if let Some(msgs) = self.messages.get_mut(&conv_id) {
                if let Some(msg) = msgs.iter_mut().find(|m| m.id == msg_id) {
                    msg.liked = has_any;
                }
            }
        }
    }

    pub(crate) fn ensure_my_documents_conv(&mut self) {
        if !self.has_session {
            return;
        }
        let Some(sid) = self
            .send2me_id
            .clone()
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        if let Some(conv) = self.convs.iter_mut().find(|c| c.thread_id == sid) {
            if conv.name != "My Documents" {
                conv.name = "My Documents".to_owned();
            }
            let id = conv.id;
            if self.server_pinned_threads.contains(&sid) {
                self.pinned.insert(id);
            }
            return;
        }
        let id = data::hash_id(&sid);
        self.convs.push(ChatConv {
            id,
            thread_id: sid.clone(),
            name: "My Documents".to_owned(),
            preview: String::new(),
            time: String::new(),
            unread: 0,
            is_group: false,
            online: false,
            live: true,
        });
        if self.server_pinned_threads.contains(&sid) {
            self.pinned.insert(id);
        }
        if self.server_archived_threads.contains(&sid) {
            self.archived.insert(id);
        }
    }

    pub(crate) fn apply_server_pinned(&mut self, raw_ids: &[String]) {
        self.server_pinned_threads = raw_ids
            .iter()
            .filter_map(|r| crate::read_sync::normalize_thread_id(r))
            .collect();
        self.ensure_my_documents_conv();
        Self::sync_live_set(&self.convs, &mut self.pinned, raw_ids);
        self.save_conv_flags();
    }

    pub(crate) fn apply_server_archived(&mut self, raw_ids: &[String]) {
        self.server_archived_threads = raw_ids
            .iter()
            .filter_map(|r| crate::read_sync::normalize_thread_id(r))
            .collect();
        Self::sync_live_set(&self.convs, &mut self.archived, raw_ids);
        self.save_conv_flags();
    }

    /// Syncs server-side muted conversations (`/api/social/profile/getmute`),
    /// cross-device unread marks (`/api/conv/getUnreadMark`), and last-seen cursors
    /// (`/api/preloadconvers/get-last-seen`) once per session in the background.
    pub(crate) fn load_server_conv_states(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) || !self.has_session {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let mut group_ids: Vec<String> = Vec::new();
        let mut direct_ids: Vec<String> = Vec::new();
        for c in self.convs.iter().take(60) {
            if !c.live || c.thread_id.is_empty() {
                continue;
            }
            if c.is_group {
                group_ids.push(c.thread_id.clone());
            } else {
                direct_ids.push(c.thread_id.clone());
            }
        }
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        let muted_val = api.get_mute_conversations().await.ok();
                        let unread_val = api.get_unread_marks().await.ok();
                        let block_share_val = api.get_block_sharing_convers().await.ok();
                        let last_seen_val = if !group_ids.is_empty() || !direct_ids.is_empty() {
                            api.get_last_seen(&group_ids, &direct_ids).await.ok()
                        } else {
                            None
                        };
                        Ok((muted_val, unread_val, last_seen_val, block_share_val))
                    })
                })
                .await;
            if let Ok((muted_val, unread_val, last_seen_val, block_share_val)) = res {
                let _ = this.update(cx, |view, cx| {
                    let mut changed = false;
                    if let Some(val) = block_share_val.as_ref() {
                        view.block_sharing = zalo_client::block_sharing_ids(val);
                        changed = true;
                    }
                    if let Some(val) = muted_val.as_ref() {
                        let muted_threads = muted_threads_from(val);
                        if !muted_threads.is_empty() {
                            Self::sync_live_set(&view.convs, &mut view.muted, &muted_threads);
                            view.save_conv_flags();
                            changed = true;
                        }
                    }
                    if let Some(val) = unread_val.as_ref() {
                        let unread_threads = unread_mark_threads_from(val);
                        let active_tid = view.selected_conv().map(|c| c.thread_id);
                        for conv in &mut view.convs {
                            if conv.live
                                && unread_threads.contains(&conv.thread_id)
                                && active_tid.as_deref() != Some(conv.thread_id.as_str())
                                && conv.unread == 0
                            {
                                conv.unread = 1;
                                changed = true;
                            }
                        }
                    }
                    if let Some(val) = last_seen_val.as_ref() {
                        for (tid, msg_id) in last_seen_from(val) {
                            if msg_id > 0 {
                                view.read_sync.mark_seen_sent(&tid, msg_id.to_string());
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Loads older messages for a group thread from Zalo's `group_cloud_message` service
    /// (`/api/cm/rgetv2`, falling back to `/api/cm/mget` when no local message exists yet).
    pub(crate) fn load_older_group_cloud(&mut self, conv_id: u64, cx: &mut Context<Self>) {
        if cfg!(test) || !self.has_session {
            return;
        }
        let Some(conv) = self.conv(conv_id).cloned().filter(|c| c.is_group && c.live) else {
            return;
        };
        let tid = conv.thread_id.clone();
        if self.cloud_history_busy.contains(&tid) || self.cloud_history_exhausted.contains(&tid) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let oldest_gmi = self
            .messages_of(conv_id)
            .iter()
            .filter_map(|m| m.global_msg_id.filter(|&id| id > 0))
            .min()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let self_uid = self.self_uid().unwrap_or_default().to_owned();
        self.cloud_history_busy.insert(tid.clone());
        let tid_req = tid.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        if oldest_gmi.is_empty() {
                            let val = api
                                .group_cloud_mget(std::slice::from_ref(&tid_req))
                                .await
                                .map_err(|e| e.to_string())?;
                            return Ok(cloud_group_items_from(&val, &tid_req, &self_uid));
                        }
                        if let Ok(val) = api.group_cloud_jump(&tid_req, &oldest_gmi, 50, false).await
                        {
                            let items = cloud_group_items_from(&val, &tid_req, &self_uid);
                            if !items.is_empty() {
                                return Ok(items);
                            }
                        }
                        if let Ok(val) = api.group_cloud_getpath(&tid_req, &oldest_gmi, 50, 0).await
                        {
                            let items = cloud_group_items_from(&val, &tid_req, &self_uid);
                            if !items.is_empty() {
                                return Ok(items);
                            }
                        }
                        let val = api
                            .group_cloud_pc_dl(&tid_req, &oldest_gmi, 50)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(cloud_group_items_from(&val, &tid_req, &self_uid))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.cloud_history_busy.remove(&tid);
                match res {
                    Ok(items) => {
                        let before = view.messages_of(conv_id).len();
                        for m in items {
                            view.apply_incoming(m, false, false);
                        }
                        let after = view.messages_of(conv_id).len();
                        let added = after.saturating_sub(before);
                        if added > 0 {
                            view.msg_window += added;
                            view.warm_media(conv_id);
                            view.set_note(format!("Đã tải thêm {added} tin nhắn cũ từ máy chủ"));
                        } else {
                            view.cloud_history_exhausted.insert(tid);
                            view.set_note("Đã hiển thị toàn bộ lịch sử tin nhắn trên máy chủ");
                        }
                    }
                    Err(err) => {
                        view.set_note(format!("Không tải được lịch sử cũ: {err}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Loads missing group cloud history around a quoted message (`/api/cm/getpath` with
    /// `/api/cm/rgetv2` jump fallback) and scrolls directly to the quoted bubble.
    pub(crate) fn load_group_cloud_quote(
        &mut self,
        conv_id: u64,
        quote_global_id: i64,
        quote_cli_id: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        if cfg!(test) || !self.has_session || quote_global_id <= 0 {
            return;
        }
        let Some(conv) = self.conv(conv_id).cloned().filter(|c| c.is_group && c.live) else {
            return;
        };
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let tid_req = conv.thread_id.clone();
        let gmi_str = quote_global_id.to_string();
        let self_uid = self.self_uid().unwrap_or_default().to_owned();
        self.set_note("Đang tìm tin nhắn gốc trên máy chủ…");
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        if let Ok(val) = api.group_cloud_getpath(&tid_req, &gmi_str, 50, 0).await {
                            let items = cloud_group_items_from(&val, &tid_req, &self_uid);
                            if !items.is_empty() {
                                return Ok(items);
                            }
                        }
                        let val = api
                            .group_cloud_jump(&tid_req, &gmi_str, 50, true)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(cloud_group_items_from(&val, &tid_req, &self_uid))
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match res {
                    Ok(items) if !items.is_empty() => {
                        let before = view.messages_of(conv_id).len();
                        for m in items {
                            view.apply_incoming(m, false, false);
                        }
                        let added = view.messages_of(conv_id).len().saturating_sub(before);
                        if added > 0 {
                            view.msg_window += added;
                            view.warm_media(conv_id);
                        }
                        if let Some((new_w, child_idx)) = crate::chat::quote_scroll_index(
                            view.messages_of(conv_id),
                            view.msg_window,
                            Some(quote_global_id),
                            quote_cli_id,
                        ) {
                            if new_w > view.msg_window {
                                view.msg_window = new_w;
                                view.warm_media(conv_id);
                            }
                            if let Some(c) = view.conv(conv_id).cloned() {
                                let (hidden, rows) = view.build_msg_rows(&c);
                                view.sync_msg_list(c.id, rows.len(), hidden);
                            }
                            view.msg_list.scroll_to_reveal_item(child_idx);
                            view.set_note("Đã tải tin nhắn gốc từ máy chủ");
                        } else {
                            view.set_note("Không tìm thấy tin nhắn gốc trên máy chủ");
                        }
                    }
                    _ => view.set_note("Không tìm thấy tin nhắn gốc trong lịch sử đã tải"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Scans an external chat link via `/api/message/scanurl` before opening it in the browser,
    /// matching Zalo PC `scanUrl` (`compact4490.js`). Intercepts `zalo.me/g/...` group invite
    /// links to show the native in-app preview & join dialog (`/api/group/link/ginfo|join`).
    pub(crate) fn open_url_checked(&mut self, url: String, cx: &mut Context<Self>) {
        let trimmed = url.trim().to_owned();
        if crate::dialogs::is_zalo_group_link(&trimmed) {
            self.open_group_link_preview(trimmed, cx);
            return;
        }
        if !crate::media::is_http_url(&trimmed) {
            crate::os_util::open_url(&trimmed);
            return;
        }
        let key = trimmed.to_ascii_lowercase();
        if let Some(&safe) = self.scan_url_cache.get(&key) {
            if safe {
                crate::os_util::open_url(&trimmed);
            } else {
                self.dialog = Some(crate::dialogs::Dialog::UnsafeUrl(trimmed));
                cx.notify();
            }
            return;
        }
        if cfg!(test) || !self.has_session {
            crate::os_util::open_url(&trimmed);
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            crate::os_util::open_url(&trimmed);
            return;
        };
        let url_req = trimmed.clone();
        cx.spawn(async move |this, cx| {
            let safe = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.scan_url(&url_req)
                            .await
                            .map(|r| r.is_safe)
                            .map_err(|e| e.to_string())
                    })
                })
                .await
                .unwrap_or(true);
            let _ = this.update(cx, |view, cx| {
                if view.scan_url_cache.len() >= 100 {
                    view.scan_url_cache.clear();
                }
                view.scan_url_cache.insert(key, safe);
                if safe {
                    crate::os_util::open_url(&trimmed);
                } else {
                    view.dialog = Some(crate::dialogs::Dialog::UnsafeUrl(trimmed));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Queries per-member delivered/seen cursors for `conv_id`, preferring live WebSocket
    /// `CMD_GET_STATUS_MSG` (`575`) / `CMD_GET_STATUS_MSG_GROUP` (`576`) when connected and
    /// falling back transparently to HTTP `/api/message/getStatusMsg` / `/api/group/getStatusMsg`.
    pub(crate) fn fetch_message_status(
        &mut self,
        conv_id: u64,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if cfg!(test) || !self.has_session {
            return;
        }
        let Some(conv) = self.conv(conv_id).cloned().filter(|c| c.live) else {
            return;
        };
        let tid = conv.thread_id.clone();
        if self.status_fetch_busy.contains(&tid) {
            return;
        }
        if !force && self.status_fetched.contains(&tid) {
            return;
        }
        if conv.is_group
            && self
                .members_known(&tid)
                .is_some_and(|n| n > crate::read_sync::MAX_GROUP_SEEN_MEMBERS)
        {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let mine = self.self_member_ids();
        let member_ids: Vec<String> = if conv.is_group {
            self.profiles
                .map
                .get(&tid)
                .map(|p| {
                    p.member_ids
                        .iter()
                        .filter(|uid| !uid.is_empty() && !mine.contains(uid.as_str()))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let is_group = conv.is_group;
        let tid_req = tid.clone();
        self.status_fetch_busy.insert(tid.clone());
        cx.spawn(async move |this, cx| {
            let rows = cx
                .background_executor()
                .spawn(async move {
                    // 1. Prefer WebSocket RPC 575/576 when socket is online (Zalo PC MsgSources.SOCKET).
                    let hub = crate::ws_rpc::WsRpcHub::global();
                    if hub.is_online() {
                        let cmd = if is_group {
                            zalo_client::CMD_GET_STATUS_MSG_GROUP
                        } else {
                            zalo_client::CMD_GET_STATUS_MSG
                        };
                        let req_id = hub.next_req_id(cmd);
                        let body =
                            zalo_client::get_status_msg_ws_body(&req_id, &tid_req, &member_ids, is_group);
                        let ws_res = crate::ws_rpc::async_rt().block_on(
                            hub.request(cmd, 0, &req_id, &body, 4_000),
                        );
                        if let Ok(val) = ws_res {
                            let parsed = zalo_client::parse_status_msg_response(&val);
                            if !parsed.is_empty() {
                                return Ok(parsed);
                            }
                        }
                    }
                    // 2. Fallback to HTTP `/api/message/getStatusMsg` or `/api/group/getStatusMsg`.
                    crate::bridge::block_api(&dir, |api| async move {
                        api.get_status_msg(&tid_req, &member_ids, is_group)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.status_fetch_busy.remove(&tid);
                view.status_fetched.insert(tid.clone());
                if let Ok(list) = rows {
                    let self_uid = view.self_uid().map(str::to_owned);
                    if view.read_sync.apply_member_statuses(
                        &tid,
                        is_group,
                        self_uid.as_deref(),
                        &list,
                    ) {
                        view.prefs.set("receipts", view.read_sync.cursors_json());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Toggles Block Sharing for a conversation (`/api/conv/blocksharing/updateConvers`).
    pub(crate) fn toggle_block_sharing(&mut self, conv_id: u64, cx: &mut Context<Self>) {
        let Some(conv) = self.conv(conv_id).cloned() else {
            return;
        };
        let tid = conv.thread_id.clone();
        if self.block_sharing_busy.contains(&tid) {
            return;
        }
        let next = !self.block_sharing.contains(&tid);
        if next {
            self.block_sharing.insert(tid.clone());
        } else {
            self.block_sharing.remove(&tid);
        }
        if !conv.live || !self.has_session || cfg!(test) {
            self.set_note(if next {
                "Đã bật Chặn chia sẻ"
            } else {
                "Đã tắt Chặn chia sẻ"
            });
            cx.notify();
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let is_group = conv.is_group;
        let lang = if crate::i18n::is_english() { "en" } else { "vi" }.to_owned();
        let tid_req = tid.clone();
        self.block_sharing_busy.insert(tid.clone());
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.update_block_sharing_conver(&tid_req, is_group, next, &lang)
                            .await
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.block_sharing_busy.remove(&tid);
                match res {
                    Ok(_) => view.set_note(if next {
                        "Đã bật Chặn chia sẻ cho hội thoại này"
                    } else {
                        "Đã tắt Chặn chia sẻ cho hội thoại này"
                    }),
                    Err(err) => {
                        if next {
                            view.block_sharing.remove(&tid);
                        } else {
                            view.block_sharing.insert(tid);
                        }
                        view.set_note(format!("Không cập nhật được Chặn chia sẻ: {err}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetches server-synced 1-on-1 pinned messages (`/api/friendboard/list`) once per thread
    /// and reflects matching local messages in [`Self::pinned_msgs`].
    pub(crate) fn fetch_friend_board_pins(&mut self, conv_id: u64, cx: &mut Context<Self>) {
        if cfg!(test) || !self.has_session {
            return;
        }
        let Some(conv) = self.conv(conv_id).cloned().filter(|c| !c.is_group && c.live) else {
            return;
        };
        let tid = conv.thread_id.clone();
        if !self.friend_pins_fetched.insert(tid.clone()) {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let tid_req = tid.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        api.list_friend_board_pins(&tid_req, 0)
                            .await
                            .map(|v| zalo_client::parse_pin_topics(&v))
                            .map_err(|e| e.to_string())
                    })
                })
                .await;
            if let Ok((pins, ver)) = res {
                let _ = this.update(cx, |view, cx| {
                    let gmis: HashSet<i64> = pins.iter().filter_map(|p| p.global_msg_id).collect();
                    if !gmis.is_empty() {
                        for m in view.messages_of(conv_id).to_vec() {
                            if m.global_msg_id.is_some_and(|g| gmis.contains(&g)) {
                                view.pinned_msgs.insert(m.id);
                            }
                        }
                    }
                    view.friend_pins.insert(tid, (pins, ver));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Syncs a 1-on-1 message pin/unpin to Zalo's `/api/friendboard/create` or
    /// `/api/friendboard/multi_unpin`.
    pub(crate) fn sync_friend_pin(
        &mut self,
        conv_id: u64,
        msg: &ChatMsg,
        pin: bool,
        cx: &mut Context<Self>,
    ) {
        if cfg!(test) || !self.has_session {
            return;
        }
        let Some(conv) = self.conv(conv_id).cloned().filter(|c| !c.is_group && c.live) else {
            return;
        };
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let tid = conv.thread_id.clone();
        let (existing_pins, version) = self
            .friend_pins
            .get(&tid)
            .cloned()
            .unwrap_or_else(|| (Vec::new(), 0));
        let lang = if crate::i18n::is_english() { "en" } else { "vi" }.to_owned();
        let gmi = msg.global_msg_id.unwrap_or(0);
        let cli = msg.cli_msg_id.unwrap_or(0);
        let snippet: String = msg.text.trim().chars().take(120).collect();
        let topic = if !pin {
            existing_pins
                .iter()
                .find(|p| p.global_msg_id == Some(gmi) && gmi > 0)
                .map(|p| p.raw.clone())
                .unwrap_or_else(|| {
                    serde_json::json!({
                        "global_msg_id": gmi.to_string(),
                        "cli_msg_id": cli.to_string(),
                    })
                })
        } else {
            serde_json::json!({
                "type": 1,
                "params": serde_json::json!({
                    "global_msg_id": gmi.to_string(),
                    "cli_msg_id": cli.to_string(),
                    "title": snippet,
                }).to_string(),
            })
        };
        let tid_req = tid.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::bridge::block_api(&dir, |api| async move {
                        if pin {
                            api.create_friend_board_pin(&tid_req, &topic, version, &lang)
                                .await
                                .map_err(|e| e.to_string())?;
                        } else {
                            api.multi_unpin_friend_board(&tid_req, &[topic], version, &lang)
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                        let after = api
                            .list_friend_board_pins(&tid_req, 0)
                            .await
                            .ok()
                            .map(|v| zalo_client::parse_pin_topics(&v));
                        Ok(after)
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Ok(Some((pins, ver))) = res {
                    view.friend_pins.insert(tid, (pins, ver));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn sync_live_set(convs: &[ChatConv], set: &mut HashSet<u64>, raw_ids: &[String]) {
        let threads: HashSet<String> = raw_ids
            .iter()
            .filter_map(|r| crate::read_sync::normalize_thread_id(r))
            .collect();
        set.retain(|id| !convs.iter().any(|c| c.id == *id && c.live));
        for conv in convs {
            if conv.live && threads.contains(&conv.thread_id) {
                set.insert(conv.id);
            }
        }
    }

    pub(crate) fn send_seen_if_due(&mut self, thread_id: &str) {
        if cfg!(test) || self.locked || !self.has_session || !data::live_allowed(thread_id) {
            return;
        }
        if self.prefs.bool("hideSeen", false) {
            return;
        }
        let Some((gmi, record)) = self.read_sync.seen_due(thread_id) else {
            return;
        };
        self.read_sync.mark_seen_sent(thread_id, gmi);
        self.maybe_live(LiveAction::Seen {
            thread_id: thread_id.to_owned(),
            record,
        });
    }

    /// Tell the sender this machine received the message, unless "Hide Received" is on.
    fn send_delivered_if_due(&mut self, thread_id: &str) {
        if cfg!(test) || self.locked || !self.has_session || !data::live_allowed(thread_id) {
            return;
        }
        if self.prefs.bool("hideDelivered", false) {
            return;
        }
        let Some((gmi, record)) = self.read_sync.delivered_due(thread_id) else {
            return;
        };
        self.read_sync.mark_delivered_sent(thread_id, gmi);
        self.maybe_live(LiveAction::Delivered {
            thread_id: thread_id.to_owned(),
            record,
        });
    }

    fn sync_open_seen(&mut self, active: bool) {
        if !active || self.locked || self.rail != Rail::Chats {
            return;
        }
        let Some(conv) = self.selected_conv() else {
            return;
        };
        if self.chat_scrolled_up || self.unread_above_count(conv.id) > 0 {
            return;
        }
        if conv.live {
            self.send_seen_if_due(&conv.thread_id);
        }
    }

    fn maybe_send_typing(&mut self) {
        if cfg!(test) || self.locked || !self.has_session || self.prefs.bool("hideTyping", false) {
            return;
        }
        let Some(conv) = self.selected_conv() else {
            return;
        };
        if !conv.live || self.rail != Rail::Chats {
            return;
        }
        let now = std::time::Instant::now();
        if let Some((prev_id, at)) = self.last_typing_sent {
            if prev_id == conv.id && now.duration_since(at) < std::time::Duration::from_secs(5) {
                return;
            }
        }
        self.last_typing_sent = Some((conv.id, now));
        self.maybe_live(LiveAction::Typing {
            thread_id: conv.thread_id,
            is_group: conv.is_group,
        });
    }

    fn schedule_typing_expiry(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            gpui::Timer::after(std::time::Duration::from_secs(6)).await;
            let _ = this.update(cx, |view, cx| {
                let before = view.typing.len();
                view.typing
                    .retain(|_, (_, at)| at.elapsed() < std::time::Duration::from_secs(6));
                if view.typing.len() != before {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Label when another participant in `conv` typed within the last 6 seconds.
    pub(crate) fn typing_label_for(&self, conv: &ChatConv) -> Option<String> {
        let (uid, at) = self.typing.get(&conv.thread_id)?;
        if at.elapsed() >= std::time::Duration::from_secs(6) {
            return None;
        }
        if !conv.is_group {
            return Some("Đang soạn tin...".to_owned());
        }
        let who = self
            .profiles
            .map
            .get(uid)
            .map(|p| p.name.as_str())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or("Ai đó");
        Some(format!("{who} đang soạn tin..."))
    }

    /// "Open in Separate Window": the conversation in its own window; this window
    /// shows a placeholder until "Show here".
    pub(crate) fn open_solo(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(conv) = self.conv(id).cloned() else {
            return;
        };
        self.detached.insert(id);
        let thread = conv.thread_id.clone();
        let outer = crate::window_frame::outer_size(720.0, 640.0);
        let bounds = crate::bounds_in_work_area(outer, cx);
        crate::x11_frame::anticipate_csd_window();
        let _ = cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                titlebar: None,
                window_decorations: Some(gpui::WindowDecorations::Client),
                window_background: gpui::WindowBackgroundAppearance::Transparent,
                window_min_size: Some(crate::window_frame::outer_size(420.0, 400.0)),
                app_id: Some("zalo-gpui".into()),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| ShellView::new_solo(window, cx, thread.clone())),
        );
    }

    /// One-conversation window (no list, no realtime: it re-reads
    /// `store.db`, which the main window's listener keeps current).
    pub fn new_solo(window: &mut Window, cx: &mut Context<Self>, thread_id: String) -> Self {
        let mut this = Self::new(window, cx);
        // Dev hooks (ZALO_GPUI_INFO / DIALOG / MENU…) are for the main window.
        this.info_open = false;
        this.dialog = None;
        this.menu = None;
        this.viewer = None;
        this.settings_open = false;
        this.rail_popup = None;
        this.rt_started = true;
        this.profiles_refresh = false;
        if let Some(id) = this
            .convs
            .iter()
            .find(|c| c.thread_id == thread_id)
            .map(|c| c.id)
        {
            this.select_conv(id);
            window.set_window_title(&this.conv(id).map(|c| c.name.clone()).unwrap_or_default());
        }
        this.solo = Some(thread_id);
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| loop {
                gpui::Timer::after(std::time::Duration::from_secs(3)).await;
                if weak
                    .update(cx, |view, cx| {
                        view.reload_solo();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            })
            .detach();
        this
    }

    fn reload_solo(&mut self) {
        let Some(thread) = self.solo.clone() else {
            return;
        };
        let snap = data::load_snapshot();
        let Some(conv) = snap.convs.iter().find(|c| c.thread_id == thread) else {
            return;
        };
        let fresh = snap.messages.get(&conv.id).cloned().unwrap_or_default();
        let id = conv.id;
        let old_len = self.messages_of(id).len();
        // Keep locally sent bubbles that are not in the store yet.
        let pending: Vec<ChatMsg> = self
            .messages_of(id)
            .iter()
            .filter(|m| {
                m.global_msg_id.is_none()
                    && !fresh
                        .iter()
                        .any(|f| f.cli_msg_id.is_some() && f.cli_msg_id == m.cli_msg_id)
            })
            .filter(|m| m.from_me && m.at_ms > data::now_ms() - 120_000)
            .cloned()
            .collect();
        let mut list = fresh;
        list.extend(pending);
        if list.len() != old_len {
            self.messages.insert(id, list);
            self.warm_media(id);
            self.scroll_msgs_to_bottom();
        }
    }

    /// Account-scoped `profiles.json` path bound to this shell's `session_dir`.
    fn profiles_cache_path(&self) -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        self.session_dir
            .as_ref()
            .map(|dir| dir.join("profiles.json"))
    }

    /// Reloads conversations and messages from `LocalStore` (called after Sync 2 persists batches),
    /// preserving the currently selected conversation thread.
    /// Reloads `store.db` on a background thread so large stores (100 MB+) never block UI input.
    ///
    /// Overlapping calls while a reload is already running are coalesced into one follow-up pass.
    pub(crate) fn reload_from_store_async(&mut self, full_refresh: bool, cx: &mut Context<Self>) {
        if self.reload_store_inflight {
            self.reload_store_pending = Some(
                self.reload_store_pending
                    .map_or(full_refresh, |prev| prev || full_refresh),
            );
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        self.reload_store_inflight = true;
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { data::load_snapshot_from_dir(&dir) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.reload_store_inflight = false;
                if let Some(snap) = loaded {
                    view.apply_reloaded_snapshot(snap, full_refresh);
                }
                if let Some(next_full) = view.reload_store_pending.take() {
                    view.reload_from_store_async(next_full, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_reloaded_snapshot(&mut self, snap: data::Snapshot, full_refresh: bool) {
        let prev_thread = self
            .selected
            .and_then(|id| self.convs.iter().find(|c| c.id == id))
            .map(|c| c.thread_id.clone());
        let prev_msg_count = self
            .selected
            .map(|id| self.messages_of(id).len())
            .unwrap_or(0);
        let was_at_bottom = self.is_msgs_at_bottom();
        if snap.user_id.as_deref().is_some_and(|s| !s.is_empty()) {
            self.user_id = snap.user_id;
        }
        if snap.send2me_id.as_deref().is_some_and(|s| !s.is_empty()) {
            self.send2me_id = snap.send2me_id;
        }
        self.convs = snap.convs;
        self.messages = snap.messages;
        self.next_msg_id = self.next_msg_id.max(snap.next_msg_id);
        self.ensure_my_documents_conv();
        self.load_conv_flags();
        if let Some(tid) = prev_thread {
            self.selected = self
                .convs
                .iter()
                .find(|c| c.thread_id == tid)
                .map(|c| c.id)
                .or_else(|| initial_thread(&self.convs));
        } else if self.selected.is_none() {
            self.selected = initial_thread(&self.convs);
        }
        self.apply_profile_aliases();
        self.msg_list_state.set((None, 0, 0));
        if full_refresh {
            self.avatar_miss.clear();
            self.group_gaps_tried.clear();
            self.profile_uids_tried.clear();
            self.media_shelf = crate::media_store::Shelf::default();
            self.profile_gaps_started = false;
            self.avatars_queued = true;
        }
        if let Some(id) = self.selected {
            self.warm_media(id);
            if full_refresh && self.info_open {
                self.warm_info_media(id);
            }
            if prev_msg_count == 0 || was_at_bottom {
                self.scroll_msgs_to_bottom();
            }
        }
        if full_refresh && self.has_session && self.profiles.stale(data::now_ms()) {
            self.profiles_refresh = true;
        }
    }

    /// Force the next render to refetch profiles (new group / friend).
    pub(crate) fn profiles_stale(&mut self) {
        self.profiles.saved_at = 0;
        self.profiles_refresh = self.has_session;
    }

    /// Clipboard image → temp PNG/JPEG → photo tray under the composer.
    pub(crate) fn paste_image(&mut self, image: &gpui::Image) {
        let ext = match image.format {
            gpui::ImageFormat::Jpeg => "jpg",
            gpui::ImageFormat::Gif => "gif",
            gpui::ImageFormat::Webp => "webp",
            gpui::ImageFormat::Bmp => "bmp",
            _ => "png",
        };
        let path = std::env::temp_dir().join(format!("zalo-gpui-paste-{}.{ext}", data::now_ms()));
        match std::fs::write(&path, image.bytes()) {
            Ok(()) => {
                self.set_pending_photo(path);
                self.set_note("Đã dán ảnh — nhấn Enter để gửi");
            }
            Err(e) => self.set_note(format!("Không dán được ảnh: {e}")),
        }
    }

    /// Open the Electron photo popup (window title `Zalo Photo`).
    ///
    /// The in-shell overlay stays only when the window cannot be created.
    pub(crate) fn open_viewer(
        &mut self,
        msg: ChatMsg,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_photo(msg, false, cx);
    }

    /// Gallery window for a photo, video, GIF or doodle. `start_ocr` is the
    /// context-menu "Scan Text" path.
    pub(crate) fn open_photo(&mut self, msg: ChatMsg, start_ocr: bool, cx: &mut Context<Self>) {
        let name = self
            .conv(msg.conv)
            .map(|c| c.name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| msg.sender.clone());
        let mut items: Vec<ChatMsg> = self
            .messages
            .get(&msg.conv)
            .map(|msgs| {
                msgs.iter()
                    .filter(|m| {
                        !m.recalled
                            && matches!(
                                m.kind,
                                crate::media::MsgKind::Photo
                                    | crate::media::MsgKind::Doodle
                                    | crate::media::MsgKind::Gif
                                    | crate::media::MsgKind::Video
                            )
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if !items.iter().any(|m| m.id == msg.id) {
            items.push(msg.clone());
        }
        let clicked = msg.id;
        items.retain(|item| item.id == clicked || crate::photo_window::has_gallery_source(item));
        if items.is_empty() {
            items.push(msg.clone());
        }
        let id = msg.id;
        let mut sender_avatars = std::collections::HashMap::new();
        let mut initial_thumbs = std::collections::HashMap::new();
        for item in &items {
            let av = if item.from_me {
                self.avatar_of(crate::profiles::ME)
            } else if !item.sender_uid.is_empty() {
                self.avatar_of(&item.sender_uid)
            } else {
                None
            };
            if let Some(img) = av {
                sender_avatars.insert(item.id, img);
            }
            if let Some(thumb) = self.media_bitmaps.get(&item.id).cloned() {
                initial_thumbs.insert(item.id, thumb);
            }
        }
        let opened = crate::photo_window::open(
            cx,
            name,
            items,
            id,
            sender_avatars,
            initial_thumbs,
            self.download_dir(),
            self.session_dir.clone(),
            start_ocr,
            self.prefs.bool("muteVideo", false),
        );
        if opened {
            // The popup owns the keyboard. Drop a leftover overlay so Escape
            // and the arrows are not swallowed by the chat window.
            self.viewer = None;
            self.viewer_drag = None;
            self.viewer_dragged = false;
        } else {
            self.viewer = Some(Viewer::new(msg));
            self.viewer_drag = None;
            self.viewer_dragged = false;
            self.reload_viewer(cx);
        }
    }

    /// Avatar bitmap for a thread id / uid / [`crate::profiles::ME`].
    /// Group rows are stored as `g_<id>` and also as the bare id.
    pub(crate) fn avatar_of(&self, key: &str) -> Option<Arc<RenderImage>> {
        self.avatars.get(key).cloned().or_else(|| {
            let alt = alt_profile_key(key);
            self.avatars.get(&alt).cloned()
        })
    }

    /// Group member count from the server profile, when known.
    pub(crate) fn members_known(&self, thread_id: &str) -> Option<u32> {
        self.profiles
            .map
            .get(thread_id)
            .map(|p| p.members)
            .filter(|n| *n > 0)
    }

    /// Kick the daily profile refresh and the avatar decode (needs `cx`).
    fn drain_profiles(&mut self, cx: &mut Context<Self>) {
        if self.profiles_refresh {
            self.profiles_refresh = false;
            if let Some(dir) = self.session_dir.clone() {
                self.profiles_fetching = true;
                cx.spawn(async move |this, cx| {
                    let now = data::now_ms();
                    let result = cx
                        .background_executor()
                        .spawn(async move { crate::profiles::fetch(&dir, now) })
                        .await;
                    let _ = this.update(cx, |view, cx| {
                        view.profiles_fetching = false;
                        // The fresh map replaced whatever a gap pass had merged.
                        view.profile_gaps_started = false;
                        match result {
                            Ok(p) => {
                                if view.display_name.is_none() {
                                    view.display_name = p
                                        .map
                                        .get(crate::profiles::ME)
                                        .map(|m| m.name.clone())
                                        .filter(|n| !n.is_empty());
                                }
                                view.profiles.merge(p);
                                if let Some(path) = view.profiles_cache_path() {
                                    view.profiles.save(&path);
                                }
                                view.avatar_miss.clear();
                                view.apply_profile_aliases();
                                view.avatars_queued = true;
                                view.refresh_notice_avatar(cx);
                            }
                            Err(err) => {
                                eprintln!("zalo-gpui profiles: {err}");
                                if crate::login_flow::is_session_error(&err) {
                                    view.kick_to_login = true;
                                }
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        if !std::mem::take(&mut self.avatars_queued) {
            return;
        }
        // Friends first while Danh bạ is open, so the visible rows get photos
        // before the rest of the cache. Then threads, recent senders, me.
        let mut keys: Vec<String> = Vec::with_capacity(self.convs.len() + 64);
        let mut key_set: HashSet<String> = HashSet::with_capacity(self.convs.len() + 64);
        let mut push_key = |k: String| {
            if !k.is_empty() && key_set.insert(k.clone()) {
                keys.push(k);
            }
        };
        if self.rail == Rail::Contacts {
            let groups = self.contact_tab == crate::sidebar::ContactTab::Groups;
            for (uid, _, _) in self.contact_rows(groups) {
                push_key(uid);
            }
        }
        for c in &self.convs {
            push_key(c.thread_id.clone());
        }
        push_key(crate::profiles::ME.into());
        if self.avatar_all {
            for k in self.profiles.map.keys() {
                push_key(k.clone());
            }
        }
        // Groups with no photo of their own use up to four member photos,
        // the same collage Zalo PC paints instead of a letter. The open
        // group still needs every member face in the info column, even
        // when the group itself already has a photo.
        let open_group = self.selected.and_then(|id| {
            self.convs
                .iter()
                .find(|c| c.id == id && c.is_group)
                .map(|c| c.thread_id.clone())
        });
        for conv in &self.convs {
            if !conv.is_group {
                continue;
            }
            let own_photo = profile_avatar(&self.profiles, &conv.thread_id)
                .is_some_and(|url| crate::sidebar::is_real_avatar(&url));
            let open_here = open_group.as_deref() == Some(conv.thread_id.as_str());
            if own_photo && !open_here {
                continue;
            }
            let Some(profile) = self.profile_ref(&conv.thread_id) else {
                continue;
            };
            if open_here {
                for uid in profile.member_ids.iter().take(40) {
                    push_key(uid.clone());
                }
            } else {
                for uid in self.group_collage_uids(&conv.thread_id) {
                    push_key(uid);
                }
            }
        }
        for msgs in self.messages.values() {
            for m in msgs.iter().rev().take(80) {
                if !m.sender_uid.is_empty() {
                    push_key(m.sender_uid.clone());
                }
            }
        }
        if let Some(key) = self.notice_thread.clone() {
            if self.shown_photo(&key).is_none() {
                if key.starts_with("g_") {
                    for uid in self.group_collage_uids(&key).into_iter().rev() {
                        keys.retain(|item| item != &uid);
                        keys.insert(0, uid);
                    }
                }
                keys.retain(|item| item != &key);
                keys.insert(0, key);
            }
        }
        let pending: Vec<(String, String)> = keys
            .into_iter()
            .filter(|k| !self.avatars.contains_key(k) && !self.avatar_miss.contains(k))
            .filter_map(|k| {
                let url = profile_avatar(&self.profiles, &k)?;
                crate::sidebar::is_real_avatar(&url).then_some((k, url))
            })
            .collect();
        // One batch paints before the next starts. A full friend list is
        // hundreds of decodes; waiting for all of them left every row a letter.
        const AVATAR_CHUNK: usize = 16;
        let more = pending.len() > AVATAR_CHUNK;
        let jobs: Vec<(String, crate::fetch::Job)> = pending
            .into_iter()
            .take(AVATAR_CHUNK)
            .enumerate()
            .map(|(i, (k, url))| {
                (
                    k,
                    crate::fetch::Job {
                        msg_id: i as u64,
                        path: None,
                        url,
                        max_edge: 48,
                    },
                )
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let dir = self.session_dir.clone();
        cx.spawn(async move |this, cx| {
            let (keys, jobs): (Vec<String>, Vec<crate::fetch::Job>) = jobs.into_iter().unzip();
            let done = cx
                .background_executor()
                .spawn(async move { crate::fetch::run_batch(dir.as_deref(), jobs) })
                .await;
            let _ = this.update(cx, |view, cx| {
                let mut got = 0usize;
                for d in done {
                    let Some(key) = keys.get(d.msg_id as usize) else {
                        continue;
                    };
                    if let Some(img) = d.image {
                        view.avatars.insert(key.clone(), img);
                        got += 1;
                    } else {
                        view.avatar_miss.insert(key.clone());
                    }
                }
                eprintln!(
                    "zalo-gpui avatars: chunk {got}/{} ok, miss {}, more={more}",
                    keys.len(),
                    view.avatar_miss.len()
                );
                if more {
                    view.avatars_queued = true;
                } else if got > 0
                    && !view.avatar_miss.is_empty()
                    && !view.avatar_miss_retried
                {
                    // Early chunks may have failed during a brief startup network blip
                    // while later chunks succeeded; retry the missed keys once.
                    view.avatar_miss_retried = true;
                    view.avatar_miss.clear();
                    view.avatars_queued = true;
                }
                view.refresh_notice_avatar(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// One `getprofiles/v2` pass for senders, members and 1-1 peers that
    /// still have no https avatar. Does not replace a photo already cached.
    fn fill_profile_gaps(&mut self, cx: &mut Context<Self>) {
        if self.profile_gaps_started || self.profiles_fetching || cfg!(test) || !self.has_session {
            return;
        }
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        self.profile_gaps_started = true;
        // Prioritize visible conversations & active-thread senders ahead of
        // hundreds of offline group members so `.take(80)` never starves them.
        let mut want: Vec<String> = Vec::with_capacity(256);
        let mut seen: HashSet<String> = HashSet::with_capacity(256);
        let mut push_uid = |uid: &str| {
            let u = uid.trim();
            if !u.is_empty() && !u.starts_with("g_") && seen.insert(u.to_owned()) {
                want.push(u.to_owned());
            }
        };
        if let Some(sel_id) = self.selected {
            if let Some(conv) = self.conv(sel_id) {
                push_uid(&conv.thread_id);
            }
            for msg in self.messages_of(sel_id).iter().rev().take(120) {
                push_uid(&msg.sender_uid);
            }
        }
        for conv in &self.convs {
            push_uid(&conv.thread_id);
        }
        for msgs in self.messages.values() {
            for msg in msgs.iter().rev().take(40) {
                push_uid(&msg.sender_uid);
            }
        }
        for conv in &self.convs {
            if conv.is_group {
                for uid in self.group_collage_uids(&conv.thread_id) {
                    push_uid(&uid);
                }
            }
        }
        for profile in self.profiles.map.values() {
            for uid in &profile.member_ids {
                push_uid(uid);
            }
        }
        let missing: Vec<String> = crate::profiles::missing_avatar_uids(&self.profiles, &want)
            .into_iter()
            .filter(|uid| !self.profile_uids_tried.contains(uid))
            .take(80)
            .collect();
        for uid in &missing {
            self.profile_uids_tried.insert(uid.clone());
        }
        let thread_ids: Vec<String> = self.convs.iter().map(|c| c.thread_id.clone()).collect();
        let groups: Vec<String> = crate::profiles::groups_needing_info(&self.profiles, &thread_ids)
            .into_iter()
            .filter(|id| !self.group_gaps_tried.contains(id))
            .collect();
        for id in &groups {
            self.group_gaps_tried.insert(id.clone());
        }
        if missing.is_empty() && groups.is_empty() {
            return;
        }
        let dir_groups = dir.clone();
        let dir_members = dir.clone();
        let group_count = groups.len();
        eprintln!("zalo-gpui profile gaps: {group_count} nhóm, {} người", missing.len());
        let _ = std::io::Write::flush(&mut std::io::stderr());
        cx.spawn(async move |this, cx| {
            // Group names are what the list is waiting on. Do not wait for the
            // user-profile chunks to finish first; those can take several timeouts.
            let users_task = cx
                .background_executor()
                .spawn(async move { crate::profiles::fetch_missing(&dir, &missing) });
            let groups_task = cx.background_executor().spawn(async move {
                crate::profiles::fetch_missing_groups(&dir_groups, &groups)
            });
            let got_groups = groups_task.await;
            let mut followup_uids: Vec<String> = Vec::new();
            let _ = this.update(cx, |view, cx| {
                let mut changed = false;
                let mut added_members: Vec<String> = Vec::new();
                match got_groups {
                    Ok((groups, members)) => {
                        for (id, got) in groups {
                            let slot = view.profiles.map.entry(id).or_default();
                            if !got.name.is_empty() && slot.name != got.name {
                                slot.name = got.name;
                                changed = true;
                            }
                            if (got.avatar.starts_with("https://")
                                || got.avatar.starts_with("http://"))
                                && slot.avatar != got.avatar
                            {
                                slot.avatar = got.avatar;
                                changed = true;
                            }
                            if got.members > 0 && slot.members != got.members {
                                slot.members = got.members;
                                changed = true;
                            }
                            if !got.member_ids.is_empty() && slot.member_ids != got.member_ids {
                                added_members.extend(got.member_ids.iter().take(8).cloned());
                                slot.member_ids = got.member_ids;
                                changed = true;
                            }
                            if !got.creator_id.is_empty() && slot.creator_id != got.creator_id {
                                slot.creator_id = got.creator_id;
                                changed = true;
                            }
                            if !got.admin_ids.is_empty() && slot.admin_ids != got.admin_ids {
                                slot.admin_ids = got.admin_ids;
                                changed = true;
                            }
                        }
                        for (uid, got) in members {
                            if let std::collections::hash_map::Entry::Vacant(slot) =
                                view.profiles.map.entry(uid)
                            {
                                slot.insert(got);
                                changed = true;
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("zalo-gpui group gaps: {err}");
                        if crate::login_flow::is_session_error(&err) {
                            view.kick_to_login = true;
                        }
                    }
                }
                if !added_members.is_empty() {
                    followup_uids =
                        crate::profiles::missing_avatar_uids(&view.profiles, &added_members)
                            .into_iter()
                            .take(40)
                            .collect();
                }
                if changed {
                    view.apply_profile_aliases();
                    if let Some(path) = view.profiles_cache_path() {
                        view.profiles.save(&path);
                    }
                    view.avatar_miss.clear();
                    view.avatars_queued = true;
                    view.refresh_notice_avatar(cx);
                }
                cx.notify();
            });
            if !followup_uids.is_empty() {
                let extra = cx
                    .background_executor()
                    .spawn(async move { crate::profiles::fetch_missing(&dir_members, &followup_uids) })
                    .await;
                if let Ok(map) = extra {
                    let _ = this.update(cx, |view, cx| {
                        let mut changed = false;
                        for (uid, got) in map {
                            let slot = view.profiles.map.entry(uid.clone()).or_default();
                            if (got.avatar.starts_with("https://")
                                || got.avatar.starts_with("http://"))
                                && !crate::sidebar::is_real_avatar(&slot.avatar)
                            {
                                slot.avatar = got.avatar;
                                view.avatar_miss.remove(&uid);
                                changed = true;
                            }
                            if slot.name.is_empty() && !got.name.is_empty() {
                                slot.name = got.name;
                                changed = true;
                            }
                        }
                        if changed {
                            view.apply_profile_aliases();
                            if let Some(path) = view.profiles_cache_path() {
                                view.profiles.save(&path);
                            }
                            view.avatars_queued = true;
                            view.refresh_notice_avatar(cx);
                            cx.notify();
                        }
                    });
                }
            }
            let got = users_task.await;
            let _ = this.update(cx, |view, cx| {
                let mut changed = false;
                match got {
                    Ok(map) => {
                        for (uid, got) in map {
                            let slot = view.profiles.map.entry(uid.clone()).or_default();
                            if (got.avatar.starts_with("https://")
                                || got.avatar.starts_with("http://"))
                                && !crate::sidebar::is_real_avatar(&slot.avatar)
                            {
                                slot.avatar = got.avatar;
                                view.avatar_miss.remove(&uid);
                                changed = true;
                            }
                            if slot.name.is_empty() && !got.name.is_empty() {
                                slot.name = got.name;
                                changed = true;
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("zalo-gpui profile gaps: {err}");
                        if crate::login_flow::is_session_error(&err) {
                            view.kick_to_login = true;
                        }
                    }
                }
                if changed {
                    view.apply_profile_aliases();
                    if let Some(path) = view.profiles_cache_path() {
                        view.profiles.save(&path);
                    }
                    view.avatars_queued = true;
                    view.refresh_notice_avatar(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Fill the open popup once this thread's title, photo, or group collage updates.
    fn refresh_notice_avatar(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.notice_thread.clone() else {
            return;
        };
        let is_group = key.starts_with("g_")
            || self
                .convs
                .iter()
                .any(|c| c.thread_id == key && c.is_group);
        let title = if is_group {
            let current = self
                .convs
                .iter()
                .find(|c| c.thread_id == key)
                .map(|c| c.name.as_str())
                .unwrap_or("");
            let prof_name = self
                .profile_ref(&key)
                .map(|p| p.name.as_str())
                .unwrap_or("");
            let resolved = crate::profiles::group_display_name(&key, current, prof_name);
            (!resolved.is_empty()
                && resolved != "Nhóm"
                && !crate::profiles::is_id_label(&key, &resolved))
            .then_some(resolved)
        } else {
            None
        };
        let image = self.shown_photo(&key);
        let collage = if is_group && image.is_none() {
            self.group_collage_faces(&key)
        } else {
            Vec::new()
        };
        if title.is_none() && image.is_none() && collage.is_empty() {
            return;
        }
        if let Some(pending) = self.pending_notice.as_mut() {
            if pending.thread_key == key {
                if let Some(name) = title.as_ref() {
                    pending.initial = crate::sidebar::avatar_initials(name);
                    pending.title = name.clone();
                }
                if let Some(img) = image.as_ref() {
                    if pending.image.is_none() {
                        pending.image = Some(img.clone());
                        pending.collage.clear();
                    }
                } else if pending.image.is_none() && !collage.is_empty() {
                    pending.collage = collage.clone();
                }
            }
        }
        crate::notice::patch_notice(cx, &key, title, image, collage);
    }

    /// Profile stored under `key` or its `g_` / bare twin.
    fn profile_ref(&self, key: &str) -> Option<&crate::profiles::Profile> {
        self.profiles
            .map
            .get(key)
            .or_else(|| self.profiles.map.get(&alt_profile_key(key)))
    }

    /// Bitmap Electron would paint. A URL containing `/default` is not a photo.
    pub(crate) fn shown_photo(&self, id: &str) -> Option<Arc<RenderImage>> {
        if self
            .profile_ref(id)
            .is_some_and(|profile| !crate::sidebar::is_real_avatar(&profile.avatar))
        {
            return None;
        }
        self.avatar_of(id)
    }

    /// Exact member UIDs that [`Self::group_collage_faces`] displays for `thread_id`.
    fn group_collage_uids(&self, thread_id: &str) -> Vec<String> {
        let Some(profile) = self.profile_ref(thread_id) else {
            return Vec::new();
        };
        let total = if profile.members > 0 {
            profile.members
        } else {
            u32::try_from(profile.member_ids.len()).unwrap_or(u32::MAX)
        };
        let me = self.self_uid();
        crate::sidebar::collage_picks(
            &profile.member_ids,
            total,
            me,
            &profile.creator_id,
            &profile.admin_ids,
        )
        .into_iter()
        .filter_map(|pick| match pick {
            crate::sidebar::CollagePick::Face(i) => profile.member_ids.get(i).cloned(),
            _ => None,
        })
        .collect()
    }

    /// Overlapping collage cells for a group that has no photo of its own.
    /// Empty when the group photo is real and already decoded, so that photo
    /// stays a single image on the list, the header, and the info panel.
    pub(crate) fn group_collage_faces(&self, thread_id: &str) -> Vec<crate::sidebar::CollageFace> {
        let Some((avatar, ids, members, creator, admins)) = self.profile_ref(thread_id).map(|profile| {
            (
                profile.avatar.clone(),
                profile.member_ids.clone(),
                profile.members,
                profile.creator_id.clone(),
                profile.admin_ids.clone(),
            )
        }) else {
            return Vec::new();
        };
        if crate::sidebar::is_real_avatar(&avatar) && self.avatar_of(thread_id).is_some() {
            return Vec::new();
        }
        let total = if members > 0 {
            members
        } else {
            u32::try_from(ids.len()).unwrap_or(u32::MAX)
        };
        let me = self.self_uid().map(str::to_owned);
        crate::sidebar::collage_picks(&ids, total, me.as_deref(), &creator, &admins)
            .into_iter()
            .map(|pick| match pick {
                crate::sidebar::CollagePick::Face(index) => {
                    self.member_collage_face(ids.get(index).map(String::as_str).unwrap_or(""))
                }
                crate::sidebar::CollagePick::Count(n) => crate::sidebar::CollageFace::Count(n),
                crate::sidebar::CollagePick::Placeholder => crate::sidebar::CollageFace::Placeholder,
            })
            .collect()
    }

    /// One collage cell: a decoded photo, otherwise initials coloured by user id.
    fn member_collage_face(&self, uid: &str) -> crate::sidebar::CollageFace {
        let (name, url) = self
            .profile_ref(uid)
            .map(|profile| (profile.name.clone(), profile.avatar.clone()))
            .unwrap_or_default();
        if crate::sidebar::is_real_avatar(&url) {
            if let Some(image) = self.avatar_of(uid) {
                return crate::sidebar::CollageFace::Photo(image);
            }
        }
        crate::sidebar::CollageFace::Letter {
            text: crate::sidebar::avatar_initials(&name),
            fill: crate::sidebar::avatar_color(uid),
        }
    }

    /// Load labels once, on the first menu that shows them (user click).
    pub(crate) fn ensure_labels(&mut self) {
        if self.labels_state != LabelsState::Idle {
            return;
        }
        if !self.has_session {
            self.labels_state = LabelsState::Failed("chưa đăng nhập".into());
            return;
        }
        if self.session_dir.is_some() {
            self.labels_state = LabelsState::Loading;
            self.labels_pending = true;
        }
    }

    /// Spawn the label fetch queued by [`Self::ensure_labels`] (needs `cx`).
    pub(crate) fn drain_labels(&mut self, cx: &mut Context<Self>) {
        if !self.labels_pending {
            return;
        }
        self.labels_pending = false;
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::bridge::fetch_labels(&dir) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(labels) => {
                        view.labels = labels;
                        view.labels_state = LabelsState::Ready;
                    }
                    Err(err) => {
                        eprintln!("zalo-gpui labels: {err}");
                        view.labels_state = LabelsState::Failed(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Placeholder row text while labels are not shown.
    pub(crate) fn labels_status(&self) -> String {
        match &self.labels_state {
            LabelsState::Idle | LabelsState::Loading => "Đang tải thẻ phân loại…".into(),
            LabelsState::Ready => "Chưa có thẻ phân loại".into(),
            LabelsState::Failed(_) => "Không tải được thẻ phân loại".into(),
        }
    }

    /// Label of one conversation: local override, else the server list.
    pub(crate) fn conv_label(&self, id: u64) -> Option<i64> {
        if let Some(label) = self.conv_labels.get(&id) {
            return Some(*label);
        }
        let conv = self.conv(id)?;
        self.thread_label(&conv.thread_id)
    }

    /// Label of one thread id (`uid` or `g_<grid>`), honoring local overrides.
    pub(crate) fn thread_label(&self, thread_id: &str) -> Option<i64> {
        if let Some(conv) = self.convs.iter().find(|c| c.thread_id == thread_id) {
            if let Some(label) = self.conv_labels.get(&conv.id) {
                return Some(*label);
            }
        }
        let key = match thread_id.strip_prefix("g_") {
            Some(rest) => format!("g{rest}"),
            None => thread_id.to_owned(),
        };
        self.labels
            .iter()
            .find(|label| label.conversations.contains(&key))
            .map(|label| label.id)
    }

    /// "Copy Image": put the decoded bubble image on the clipboard.
    pub(crate) fn copy_image(&mut self, msg: &ChatMsg, cx: &mut Context<Self>) {
        let Some(path) =
            crate::media::local_media_file(msg.media_path.as_deref(), &msg.media_url, &msg.href)
        else {
            self.set_note("Ảnh chưa có trên máy để copy");
            return;
        };
        match crate::clip::encode_image_file(&path) {
            Ok((format, bytes)) => {
                crate::clip::copy_image(cx, format, bytes);
                self.set_note("Đã copy hình ảnh");
            }
            Err(err) => self.set_note(err),
        }
    }

    /// "Save to Device": copy the local file / cache into `~/Downloads`.
    pub(crate) fn save_media(&mut self, msg: &ChatMsg) {
        let Some(src) =
            crate::media::local_media_file(msg.media_path.as_deref(), &msg.media_url, &msg.href)
        else {
            self.set_note("Tệp chưa có trên máy — không có URL tải trong store.db");
            return;
        };
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let _ = &home;
        let dir = self.download_dir();
        let _ = std::fs::create_dir_all(&dir);
        let name = if msg.kind == crate::media::MsgKind::File && !msg.file_name.is_empty() {
            msg.file_name.clone()
        } else {
            let ext = std::fs::read(&src)
                .ok()
                .and_then(|b| image::guess_format(&b).ok())
                .and_then(|f| f.extensions_str().first().copied())
                .unwrap_or("jxl");
            format!("zalo-{}.{ext}", msg.id)
        };
        let dest = dir.join(name);
        match std::fs::copy(&src, &dest) {
            Ok(_) => self.set_note(format!("Đã lưu {}", dest.display())),
            Err(err) => self.set_note(err.to_string()),
        }
    }

    /// Open the folder that holds a file bubble (`STR_VIEW_IN_FOLDER`).
    pub(crate) fn reveal_file(
        &mut self,
        name: &str,
        local: Option<&std::path::Path>,
        _cx: &mut Context<Self>,
    ) {
        let found = local
            .filter(|p| p.is_file())
            .map(std::path::Path::to_path_buf)
            .or_else(|| crate::media::find_downloaded(name));
        let Some(path) = found else {
            self.set_note(format!("Chưa có «{name}» trên máy"));
            return;
        };
        let dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or(path);
        match crate::os_util::open_external(&dir) {
            Ok(()) => self.set_note(
                crate::color::str_vi("STR_VIEW_IN_FOLDER", "Hiển thị trong thư mục").to_owned(),
            ),
            Err(err) => self.set_note(err.to_string()),
        }
    }

    /// Copy a local file into the download folder (`STR_DOWNLOAD_FILE`).
    pub(crate) fn save_file_download(
        &mut self,
        name: &str,
        local: Option<&std::path::Path>,
        _cx: &mut Context<Self>,
    ) {
        let found = local
            .filter(|p| p.is_file())
            .map(std::path::Path::to_path_buf)
            .or_else(|| crate::media::find_downloaded(name));
        let Some(src) = found else {
            self.set_note(format!(
                "Chưa có «{name}» trên máy — store.db không lưu URL tải tệp nhận."
            ));
            return;
        };
        let dir = self.download_dir();
        let _ = std::fs::create_dir_all(&dir);
        let file_name = if name.trim().is_empty() {
            src.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".into())
        } else {
            name.to_owned()
        };
        let dest = dir.join(&file_name);
        if dest == src {
            self.set_note(
                crate::color::str_vi("STR_FILE_DOWNLOAD_SUCCESS_STATUS", "Đã có trên máy")
                    .to_owned(),
            );
            return;
        }
        match std::fs::copy(&src, &dest) {
            Ok(_) => self.set_note(format!("Đã lưu {}", dest.display())),
            Err(err) => self.set_note(err.to_string()),
        }
    }

    /// Open a file bubble: the local copy we sent, else Zalo PC / XDG download
    /// folders by name. `store.db` keeps no download URL for received files.
    pub(crate) fn open_file(
        &mut self,
        name: &str,
        local: Option<&std::path::Path>,
        cx: &mut Context<Self>,
    ) {
        let found = local
            .filter(|p| p.is_file())
            .map(std::path::Path::to_path_buf)
            .or_else(|| crate::media::find_downloaded(name));
        match found {
            Some(path) => {
                cx.open_with_system(&path);
                self.note = None;
            }
            None => self.set_note(format!(
                "Chưa có «{name}» trên máy — store.db không lưu URL tải tệp nhận (cần đồng bộ lại tin)."
            )),
        }
    }

    pub(crate) fn cloud_id(&self) -> Option<u64> {
        self.convs
            .iter()
            .find(|row| {
                is_my_documents(&row.name, &row.thread_id, self.send2me_id.as_deref())
            })
            .map(|row| row.id)
            .or_else(|| (!self.has_session).then(preview::cloud_of_mine_id))
    }

    /// Numeric Zalo user id (`uidMe`) of the logged-in account (`session.user_id`,
    /// falling back to `send2me_id`), distinct from `uin` which holds the 32-char `dkey`.
    pub(crate) fn self_uid(&self) -> Option<&str> {
        self.user_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.send2me_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
    }

    pub(crate) fn conv(&self, id: u64) -> Option<&ChatConv> {
        self.convs.iter().find(|row| row.id == id)
    }

    pub(crate) fn selected_conv(&self) -> Option<ChatConv> {
        self.selected.and_then(|id| self.conv(id).cloned())
    }

    pub(crate) fn unread_of(&self, row: &ChatConv) -> u32 {
        if self.marked_read.contains(&row.id) {
            0
        } else {
            row.unread
        }
    }

    fn conv_visible_in_tab(&self, row: &ChatConv, query: &str, pin_match: bool) -> bool {
        // Hidden chats stay out of the list unless unlocked, exact full-name
        // search matches, or the user enters the 4-digit hidden-chat PIN.
        if self.removed.contains(&row.id) {
            return false;
        }
        if self.conv_hidden(row.id) {
            return pin_match || (!query.is_empty() && row.name.to_lowercase() == query);
        }
        !self.prefs.bool("split", true) || self.archived.contains(&row.id) == self.other_tab
    }

    pub(crate) fn is_stranger(&self, conv: &ChatConv) -> bool {
        is_stranger_conv(
            conv,
            self.has_session,
            &self.profiles.map,
            self.send2me_id.as_deref(),
            self.uin.as_deref(),
        )
    }

    pub(crate) fn is_friend_req_sent(&self, uid: &str) -> bool {
        self.sent_friend_reqs.contains(uid)
            || self
                .requests
                .as_ref()
                .is_some_and(|r| r.sent.iter().any(|p| p.person.uid == uid))
    }

    /// `(stranger_count, unread_sum, latest_time)` for the current tab's folded folder.
    pub(crate) fn stranger_summary(&self) -> Option<(usize, u32, String)> {
        let mut count = 0usize;
        let mut unread = 0u32;
        let mut latest_time = String::new();
        for row in &self.convs {
            if self.conv_visible_in_tab(row, "", false) && self.is_stranger(row) {
                if count == 0 {
                    latest_time = row.time.clone();
                }
                count += 1;
                unread = unread.saturating_add(self.unread_of(row));
            }
        }
        (count > 0).then_some((count, unread, latest_time))
    }

    /// Indices into `self.convs` in display order (avoids cloning all `ChatConv`
    /// rows on every scroll/render frame).
    pub(crate) fn listed_conv_indices(&self) -> Vec<usize> {
        let raw_query = self.search_query.trim();
        let query = raw_query.to_lowercase();
        let pin_match = self.pin_matches(raw_query);
        let mut indices: Vec<usize> = self
            .convs
            .iter()
            .enumerate()
            .filter(|(_, row)| self.conv_visible_in_tab(row, &query, pin_match))
            .filter(|(_, row)| !is_left_group_name(&row.name))
            .filter(|(_, row)| {
                conv_matches_list_filter(
                    self.list_filter,
                    &query,
                    self.is_stranger(row),
                    self.unread_of(row),
                    self.conv_label(row.id),
                )
            })
            .filter(|(_, row)| {
                query.is_empty()
                    || (pin_match && self.conv_hidden(row.id))
                    || row.name.to_lowercase().contains(&query)
                    || row.preview.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        indices.sort_by_key(|&i| {
            if self.pinned.contains(&self.convs[i].id) {
                0u8
            } else {
                1
            }
        });
        indices
    }

    pub(crate) fn listed_convs(&self) -> Vec<ChatConv> {
        self.listed_conv_indices()
            .into_iter()
            .map(|i| self.convs[i].clone())
            .collect()
    }

    /// Esc: close the topmost overlay (menu → viewer → dialog → popup →
    /// settings → reply bar). Returns false when nothing was open.
    pub(crate) fn escape_overlay(&mut self) -> bool {
        if self.menu.take().is_some() || self.viewer.take().is_some() {
            return true;
        }
        if let Some(crate::dialogs::Dialog::UpdateAvailable) = self.dialog {
            if self.update_install.is_busy() {
                return true;
            }
        }
        if self.dialog.take().is_some() {
            return true;
        }
        if self.react_detail.take().is_some() {
            return true;
        }
        if self.react_picker.is_some() {
            self.close_react_picker();
            return true;
        }
        if self.pinned_expanded {
            self.pinned_expanded = false;
            return true;
        }
        if self.partial_quote_fab.take().is_some() {
            self.chat_sel.clear();
            return true;
        }
        if self.like_hold.take().is_some() {
            return true;
        }
        if self.dismiss_floating_panels() {
            return true;
        }
        if self.forward_of.take().is_some() || self.rail_popup.take().is_some() {
            return true;
        }
        if self.settings_open {
            self.close_settings();
            return true;
        }
        if self.select_msgs.take().is_some() || self.members_view.take().is_some() {
            return true;
        }
        if self.chat_sel.has_selection() {
            self.chat_sel.clear();
            return true;
        }
        if let Some(rec) = &self.voice_rec {
            rec.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            rec.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            self.set_note("Đang hủy ghi âm…");
            return true;
        }
        if self.sticker_open {
            self.sticker_open = false;
            return true;
        }
        if self.quick_menu {
            self.quick_menu = false;
            return true;
        }
        if self.stickers.inline.take().is_some() || !self.stickers.suggest_items.is_empty() {
            self.stickers.inline_stickers.clear();
            self.stickers.inline_gifs.clear();
            self.stickers.suggest_items.clear();
            return true;
        }
        if self.reply_to.take().is_some() {
            return true;
        }
        if self.list_filter == ListFilter::Strangers {
            self.list_filter = ListFilter::All;
            return true;
        }
        false
    }

    pub(crate) fn close_settings(&mut self) {
        self.settings_open = false;
        if self.rail == Rail::Settings {
            self.rail = Rail::Chats;
        }
    }

    /// True while `msg_id` is actively flashing its 2.5s post-jump highlight ring.
    #[must_use]
    pub(crate) fn is_msg_highlighted(&self, msg_id: u64) -> bool {
        self.highlight_msg.is_some_and(|(id, at)| {
            id == msg_id && at.elapsed() < std::time::Duration::from_millis(2600)
        })
    }

    /// Briefly highlights `msg_id` (2.5 s) after a jump from Quote / Search / Pin /
    /// Star / Unread / Mention and schedules an automatic repaint when it fades.
    pub(crate) fn flash_msg(&mut self, msg_id: u64, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        self.highlight_msg = Some((msg_id, now));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(2650))
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.highlight_msg == Some((msg_id, now)) {
                    view.highlight_msg = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Auto-loads older local messages (or older cloud group messages when local
    /// history is exhausted) as the user scrolls near the top of `#message-view`.
    pub(crate) fn maybe_auto_load_top(
        &mut self,
        visible_start: usize,
        count: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.chat_scrolled_up || count <= 4 || visible_start > 2 {
            return false;
        }
        if self
            .last_auto_load_top
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(350))
        {
            return false;
        }
        let Some(conv_id) = self.selected else {
            return false;
        };
        let total = self.messages_of(conv_id).len();
        if total > self.msg_window {
            self.last_auto_load_top = Some(std::time::Instant::now());
            self.msg_window = (self.msg_window + MSG_WINDOW).min(total);
            self.warm_media(conv_id);
            return true;
        }
        if let Some(conv) = self.conv(conv_id).cloned() {
            if conv.is_group
                && conv.live
                && self.has_session
                && !self.cloud_history_busy.contains(&conv.thread_id)
                && !self.cloud_history_exhausted.contains(&conv.thread_id)
            {
                self.last_auto_load_top = Some(std::time::Instant::now());
                self.load_older_group_cloud(conv_id, cx);
                return true;
            }
        }
        false
    }

    /// Number of unread messages still scrolled ABOVE the viewport in `conv_id`.
    #[must_use]
    pub(crate) fn unread_above_count(&self, conv_id: u64) -> u32 {
        self.unread_anchor
            .as_ref()
            .filter(|a| a.conv_id == conv_id)
            .map(|a| a.remaining_ids.len() as u32)
            .unwrap_or(0)
    }

    /// Row index in `msg_list` for a given `ChatMsg.id`, if currently inside the loaded window.
    fn row_ix_of_msg(&self, msg_id: u64) -> Option<usize> {
        self.msg_row_ids
            .borrow()
            .iter()
            .find_map(|&(ix, id)| (id == msg_id).then_some(ix))
    }

    /// Marks `conv_id` fully read locally and schedules `/seenv2` when live.
    pub(crate) fn clear_open_unread_and_mark_read(&mut self, conv_id: u64) {
        if let Some(anchor) = self.unread_anchor.as_mut() {
            if anchor.conv_id == conv_id {
                anchor.remaining_ids.clear();
            }
        }
        self.new_below_ids.clear();
        self.new_mention_below_ids.clear();
        self.chat_unread_below = 0;
        self.marked_read.insert(conv_id);
        let last_at = self
            .messages
            .get(&conv_id)
            .and_then(|l| l.last())
            .map(|m| m.at_ms)
            .unwrap_or(0)
            .max(data::now_ms());
        let mut live_thread = None;
        if let Some(conv) = self.convs.iter_mut().find(|c| c.id == conv_id) {
            conv.unread = 0;
            if conv.live {
                live_thread = Some(conv.thread_id.clone());
            }
        }
        if let Some(thread) = live_thread {
            self.mark_thread_read_pref(&thread, last_at);
            self.send_seen_if_due(&thread);
        }
    }

    /// Processes `#message-view` scroll events (matching official Zalo PC
    /// `processMessageInViewForUnread` + `processMessageInViewForNewMessages`):
    /// - Updates `chat_scrolled_up` (ignoring the trailing `BottomSpacer` row).
    /// - Gradually counts down unread messages above as the user scrolls up,
    ///   clearing the unread badge and sending `seen` once the first unread
    ///   message enters view.
    /// - Gradually counts down new incoming messages below as the user scrolls
    ///   back down to the bottom.
    ///
    /// Returns `true` when visible UI state changed and requires `cx.notify()`.
    pub(crate) fn handle_msg_scroll(
        &mut self,
        visible: std::ops::Range<usize>,
        count: usize,
    ) -> bool {
        if visible.is_empty() || count == 0 {
            return false;
        }
        let prev_vis = self.last_visible_range.get();
        self.last_visible_range.set((visible.start, visible.end));
        let mut changed = false;
        if prev_vis != (visible.start, visible.end) && self.partial_quote_fab.take().is_some() {
            changed = true;
        }
        // BottomSpacer occupies index `count - 1`; treat the user as at-bottom
        // whenever the last real bubble (`count - 2`) is visible.
        let scrolled_up = count > 2 && visible.end < count.saturating_sub(1);
        changed |= self.chat_scrolled_up != scrolled_up;
        self.chat_scrolled_up = scrolled_up;

        let Some(conv_id) = self.selected else {
            return changed;
        };

        // 1. Track unread messages ABOVE the viewport (`unread_anchor`).
        let mut cleared_above = false;
        if let Some(anchor) = self.unread_anchor.as_mut() {
            if anchor.conv_id == conv_id && !anchor.remaining_ids.is_empty() {
                let first_id = anchor.first_unread_msg_id;
                let first_row = self.unread_divider_row.get().or_else(|| {
                    self.msg_row_ids
                        .borrow()
                        .iter()
                        .find_map(|&(ix, id)| (id == first_id).then_some(ix))
                });
                let before_len = anchor.remaining_ids.len();
                if first_row.is_some_and(|r| visible.start <= r) {
                    anchor.remaining_ids.clear();
                    cleared_above = true;
                } else {
                    let row_map = self.msg_row_ids.borrow();
                    anchor.remaining_ids.retain(|&mid| {
                        row_map
                            .iter()
                            .find_map(|&(ix, id)| (id == mid).then_some(ix))
                            .is_none_or(|ix| ix < visible.start)
                    });
                    if anchor.remaining_ids.is_empty() {
                        cleared_above = true;
                    }
                }
                if anchor.remaining_ids.len() != before_len {
                    changed = true;
                }
            }
        }

        // 2. Track new messages BELOW the viewport (`new_below_ids` & mentions).
        if !scrolled_up {
            if !self.new_below_ids.is_empty()
                || !self.new_mention_below_ids.is_empty()
                || self.chat_unread_below > 0
            {
                self.new_below_ids.clear();
                self.new_mention_below_ids.clear();
                self.chat_unread_below = 0;
                changed = true;
            }
        } else if !self.new_below_ids.is_empty() {
            let before_len = self.new_below_ids.len();
            let row_map = self.msg_row_ids.borrow();
            self.new_below_ids.retain(|&mid| {
                row_map
                    .iter()
                    .find_map(|&(ix, id)| (id == mid).then_some(ix))
                    .is_none_or(|ix| ix >= visible.end)
            });
            self.new_mention_below_ids
                .retain(|id| self.new_below_ids.contains(id));
            self.chat_unread_below = self.new_below_ids.len() as u32;
            if self.new_below_ids.len() != before_len {
                changed = true;
            }
        }

        // 3. Synchronize sidebar unread badge + `/seenv2` read receipt.
        let above = self.unread_above_count(conv_id);
        let below = self.new_below_ids.len() as u32;
        let total_unseen = above.saturating_add(below);
        if total_unseen == 0 {
            if cleared_above || !self.marked_read.contains(&conv_id) {
                self.clear_open_unread_and_mark_read(conv_id);
                changed = true;
            }
        } else if let Some(conv) = self.convs.iter_mut().find(|c| c.id == conv_id) {
            if conv.unread != total_unseen {
                conv.unread = total_unseen;
                changed = true;
            }
            self.marked_read.remove(&conv_id);
        }
        changed
    }

    /// Scrolls up to the `--- Tin nhắn chưa đọc ---` separator / first unread
    /// message (triggered by the top-right `↑↑` button OR re-clicking the active
    /// conversation in the left sidebar) and marks the unread batch as read.
    pub(crate) fn jump_to_first_unread(&mut self) {
        let Some(conv_id) = self.selected else {
            return;
        };
        let Some(first_id) = self
            .unread_anchor
            .as_ref()
            .filter(|a| a.conv_id == conv_id)
            .map(|a| a.first_unread_msg_id)
        else {
            return;
        };
        let all = self.messages_of(conv_id);
        if let Some(pos) = all.iter().position(|m| m.id == first_id) {
            let needed = all.len().saturating_sub(pos) + 12;
            if needed > self.msg_window {
                self.msg_window = needed;
                self.warm_media(conv_id);
            }
        }
        if let Some(conv) = self.conv(conv_id).cloned() {
            let (hidden, items) = self.build_msg_rows(&conv);
            self.sync_msg_list(conv.id, items.len(), hidden);
        }
        let target_row = self
            .unread_divider_row
            .get()
            .or_else(|| self.row_ix_of_msg(first_id));
        if let Some(row_ix) = target_row {
            self.msg_list.scroll_to_reveal_item(row_ix);
        }
        if let Some(anchor) = self.unread_anchor.as_mut() {
            if anchor.conv_id == conv_id {
                anchor.remaining_ids.clear();
            }
        }
        if self.new_below_ids.is_empty() {
            self.clear_open_unread_and_mark_read(conv_id);
        } else if let Some(conv) = self.convs.iter_mut().find(|c| c.id == conv_id) {
            conv.unread = self.new_below_ids.len() as u32;
        }
    }

    /// Scrolls to the newest message at the bottom and clears any pending
    /// below-viewport unread / mention counters.
    pub(crate) fn jump_to_bottom_and_read(&mut self) {
        self.chat_scrolled_up = false;
        self.new_below_ids.clear();
        self.new_mention_below_ids.clear();
        self.chat_unread_below = 0;
        if let Some(conv_id) = self.selected {
            let above = self.unread_above_count(conv_id);
            if above == 0 {
                self.clear_open_unread_and_mark_read(conv_id);
            } else if let Some(conv) = self.convs.iter_mut().find(|c| c.id == conv_id) {
                conv.unread = above;
            }
        }
        self.scroll_msgs_to_bottom();
    }

    /// Jumps to the oldest unseen `@mention` message below the viewport.
    pub(crate) fn jump_to_next_mention_below(&mut self) {
        let Some(conv_id) = self.selected else {
            return;
        };
        if self.new_mention_below_ids.is_empty() {
            return;
        }
        let target_id = self.new_mention_below_ids.remove(0);
        if let Some(conv) = self.conv(conv_id).cloned() {
            let (hidden, items) = self.build_msg_rows(&conv);
            self.sync_msg_list(conv.id, items.len(), hidden);
        }
        if let Some(row_ix) = self.row_ix_of_msg(target_id) {
            self.msg_list.scroll_to_reveal_item(row_ix);
        }
    }

    pub(crate) fn select_conv(&mut self, id: u64) {
        let same_conv = self.selected == Some(id);
        // Official Zalo PC behavior: clicking the ALREADY-SELECTED conversation
        // in the left sidebar jumps UP to unread messages (if any remain above),
        // or back DOWN to the latest message if scrolled up.
        if same_conv {
            self.starred_open = false;
            self.compact_show_chat = true;
            self.rail = Rail::Chats;
            self.note = None;
            self.menu = None;
            if self.unread_above_count(id) > 0 {
                self.jump_to_first_unread();
            } else if self.chat_scrolled_up || !self.new_below_ids.is_empty() {
                self.jump_to_bottom_and_read();
            } else if let Some(anchor) = self.unread_anchor.as_ref().filter(|a| a.conv_id == id) {
                // Even if already marked read on initial open, re-clicking lets the user
                // jump back to the unread divider when it sits above the viewport.
                let (vis_start, _vis_end) = self.last_visible_range.get();
                let target_row = self
                    .unread_divider_row
                    .get()
                    .or_else(|| self.row_ix_of_msg(anchor.first_unread_msg_id));
                if target_row.is_some_and(|r| r < vis_start) {
                    self.jump_to_first_unread();
                } else {
                    self.scroll_msgs_to_bottom();
                }
            } else {
                self.scroll_msgs_to_bottom();
            }
            return;
        }

        // Preserve any unread count that wasn't scrolled up to on the previous conversation.
        if let Some(prev_id) = self.selected {
            let prev_unseen = self
                .unread_above_count(prev_id)
                .saturating_add(self.new_below_ids.len() as u32);
            if prev_unseen > 0 {
                if let Some(prev_conv) = self.convs.iter_mut().find(|c| c.id == prev_id) {
                    prev_conv.unread = prev_unseen;
                }
                self.marked_read.remove(&prev_id);
            }
        }

        let unread_before = self
            .convs
            .iter()
            .find(|c| c.id == id)
            .map(|c| self.unread_of(c))
            .unwrap_or(0);

        self.msg_window = MSG_WINDOW;
        self.chat_scrolled_up = false;
        self.chat_unread_below = 0;
        self.new_below_ids.clear();
        self.new_mention_below_ids.clear();
        self.unread_anchor = None;
        self.unread_divider_row.set(None);
        self.msg_row_ids.borrow_mut().clear();
        self.last_visible_range.set((0, 0));
        self.highlight_msg = None;
        self.last_auto_load_top = None;
        self.pinned_expanded = false;
        self.react_detail = None;
        self.partial_quote_fab = None;
        self.mention_idx = 0;
        self.select_msgs = None;
        self.chat_sel.clear();
        self.members_view = None;
        self.msg_search_open = false;
        self.msg_search_q.clear();
        self.msg_search_hits.clear();
        self.msg_search_idx = 0;
        self.msg_search_sender = None;
        self.msg_search_sender_label.clear();
        self.msg_search_date_from = None;
        self.msg_search_date_to = None;
        self.msg_search_popup = None;
        self.msg_search_draft_from = None;
        self.msg_search_draft_to = None;
        self.msg_search_presets_open = false;

        self.selected = Some(id);
        self.starred_open = false;
        self.compact_show_chat = true;
        self.avatars_queued = true;
        self.profile_gaps_started = false;
        self.rail = Rail::Chats;
        self.note = None;
        self.sticker_open = false;
        self.settings_open = false;
        self.menu = None;
        self.close_react_picker();
        self.reply_to = None;
        self.forward_of = None;

        // Build unread anchor when opening a conversation with unread messages.
        let mut anchored_unread = false;
        if unread_before > 0 {
            let all = self.messages_of(id);
            let mut unread_ids: Vec<u64> = all
                .iter()
                .rev()
                .filter(|m| !m.from_me)
                .take(unread_before as usize)
                .map(|m| m.id)
                .collect();
            if unread_ids.is_empty() {
                unread_ids = all
                    .iter()
                    .rev()
                    .take(unread_before as usize)
                    .map(|m| m.id)
                    .collect();
            }
            unread_ids.reverse();
            if let Some(&first_id) = unread_ids.first() {
                if let Some(pos) = all.iter().position(|m| m.id == first_id) {
                    let needed = all.len().saturating_sub(pos) + 12;
                    self.msg_window = self.msg_window.max(needed);
                }
                self.unread_anchor = Some(ConvUnreadAnchor {
                    conv_id: id,
                    first_unread_msg_id: first_id,
                    remaining_ids: unread_ids,
                    initial_count: unread_before,
                });
                anchored_unread = true;
            }
        }

        if !anchored_unread {
            self.clear_open_unread_and_mark_read(id);
        }
        self.warm_media(id);
        self.scroll_msgs_to_bottom();
    }

    /// Selects an existing thread or opens a live direct/group conversation row,
    /// switching back to the Chats rail and focusing the message composer.
    /// Shows `fallback_note` (or a self-chat notice) when opening is not allowed.
    pub(crate) fn open_chat_or_note(
        &mut self,
        thread_id: &str,
        name: &str,
        is_group: bool,
        fallback_note: &'static str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        let mine = self.self_member_ids();
        let Some(id) = open_thread_into(
            &mut self.convs,
            &mut self.messages,
            &self.profiles.map,
            thread_id,
            name,
            is_group,
            self.has_session,
            &mine,
        ) else {
            let note = if !is_group && mine.contains(thread_id.trim()) {
                "Đây là tài khoản của bạn"
            } else {
                fallback_note
            };
            self.set_note(note);
            return None;
        };
        if self.removed.remove(&id) {
            self.save_conv_flags();
        }
        self.members_view = None;
        self.set_rail(Rail::Chats);
        self.select_conv(id);
        let handle = gpui::Focusable::focus_handle(self.composer.read(cx), cx);
        window.focus(&handle);
        Some(id)
    }

    pub(crate) fn messages_of(&self, id: u64) -> &[ChatMsg] {
        self.messages
            .get(&id)
            .map(|msgs| msgs.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn send_draft(&mut self, text: &str, cx: &mut Context<Self>) {
        self.commit_caption_edit(cx);
        let text = text.trim();
        if !self.pending_photos.is_empty() {
            if self.selected_conv().is_none() {
                self.set_note("Chọn một hội thoại để gửi");
                return;
            }
            let photos = std::mem::take(&mut self.pending_photos);
            let captions = std::mem::take(&mut self.pending_captions);
            self.pending_thumbs.clear();
            self.caption_edit = None;
            let total = photos.len() as u32;
            let layout = if total > 1 { data::now_ms() } else { 0 };
            for (i, path) in photos.into_iter().enumerate() {
                let cell = captions
                    .get(i)
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty());
                let caption = match (i, cell) {
                    (0, Some(c)) if !text.is_empty() && c != text => format!("{text}\n{c}"),
                    (_, Some(c)) => c,
                    (0, None) => text.to_owned(),
                    _ => String::new(),
                };
                self.send_photo_now_at(path, &caption, i as i64, layout, total, i as u32);
            }
            return;
        }
        if text.is_empty() {
            return;
        }
        let Some(conv) = self.selected_conv() else {
            self.set_note("Chọn một hội thoại để gửi");
            return;
        };
        let now = data::now_ms();
        let id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.saturating_add(1);
        let quote = self.quote_target(&conv);
        let cli = now.max(1);
        let mut msg = ChatMsg::text_msg(
            id,
            conv.id,
            conv.thread_id.clone(),
            true,
            str_vi("STR_YOU", "Bạn").to_owned(),
            self.self_uid().unwrap_or_default().to_owned(),
            text.to_owned(),
            data::vn_hm(now),
            now,
            Some(cli),
            None,
        );
        let stored_quote = self.reply_to.as_ref().map(|reply| {
            let q_sender = if reply.from_me {
                str_vi("STR_YOU", "Bạn").to_owned()
            } else {
                reply.sender.clone()
            };
            let q_text = if reply.text.trim().is_empty() {
                "[Tin nhắn]".to_owned()
            } else {
                reply.text.clone()
            };
            msg.quote_sender = q_sender.clone();
            msg.quote_text = q_text.clone();
            msg.quote_global_id = reply.global_msg_id;
            msg.quote_cli_id = reply.cli_msg_id;
            data::StoredQuote {
                sender: q_sender,
                text: q_text,
                global_id: reply.global_msg_id,
                cli_id: reply.cli_msg_id,
            }
        });
        let urgency = self.urgency;
        self.urgency = 0;
        if urgency > 0 {
            self.urgency_msgs.insert(msg.id, urgency);
        }
        self.push_message(msg);
        self.reply_to = None;
        self.sticker_open = false;
        if conv.live && self.has_session {
            if let Some(dir) = self.session_dir.clone() {
                let mentions = if conv.is_group && text.contains('@') {
                    let members = self.group_member_uid_names(&conv);
                    build_group_mentions(text, &members)
                } else {
                    Vec::new()
                };
                let language = if self.english { "en" } else { "vi" }.to_owned();
                bridge::spawn_live(
                    LiveAction::Send {
                        thread_id: conv.thread_id.clone(),
                        is_group: conv.is_group,
                        text: text.to_owned(),
                        quote,
                        mentions,
                        language,
                        urgency,
                        client_id: cli as u64,
                    },
                    dir,
                );
            }
            data::persist_local_message_quote(
                &conv.thread_id,
                text,
                true,
                now,
                cli,
                stored_quote.as_ref(),
            );
        }
    }

    /// Maps press-and-hold duration on the quick-like button to `(tier, preview_px)`:
    /// - `< 220 ms`: tier 1 (normal 36 px bubble)
    /// - `220..650 ms`: tier 2 (medium 54 px bubble)
    /// - `>= 650 ms`: tier 3 (large 74 px bubble)
    #[must_use]
    pub(crate) fn like_hold_preview_px(elapsed_ms: u128) -> (u8, f32) {
        if elapsed_ms < 220 {
            let t = elapsed_ms as f32 / 220.0;
            (1, 28.0 + t * 10.0)
        } else if elapsed_ms < 650 {
            let t = (elapsed_ms - 220) as f32 / 430.0;
            (2, 38.0 + t * 20.0)
        } else {
            let t = ((elapsed_ms - 650).min(450)) as f32 / 450.0;
            (3, (58.0 + t * 22.0).min(80.0))
        }
    }

    /// Begins a press-and-hold gesture on the composer's quick-like button,
    /// repainting smoothly at ~30 FPS so the growing yellow 👍 preview scales live.
    pub(crate) fn start_like_hold(
        &mut self,
        pos: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.like_hold = Some((std::time::Instant::now(), pos));
        cx.spawn(async move |this, cx| {
            for _ in 0..90 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(30))
                    .await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        if this.like_hold.is_some() {
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
    }

    /// Finishes a press-and-hold on the quick-like button and sends `"/-strong"`
    /// at the chosen size tier (`1` normal, `2` medium, `3` large).
    pub(crate) fn finish_like_hold(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((started, pos)) = self.like_hold.take() else {
            return;
        };
        let (scale, _) = Self::like_hold_preview_px(started.elapsed().as_millis());
        if scale > 1 && self.selected_conv().is_some() && self.pending_photos.is_empty() {
            self.big_like_sizes.insert(self.next_msg_id, scale);
        }
        self.send_draft("/-strong", cx);
        self.spawn_react_burst(pos, 0, window, cx);
    }

    /// Dismisses any open non-modal floating popover/panel when clicking outside it.
    /// Returns `true` if at least one floating panel was open and closed.
    pub(crate) fn dismiss_floating_panels(&mut self) -> bool {
        let mut changed = false;
        if self.sticker_open {
            self.sticker_open = false;
            changed = true;
        }
        if self.quick_menu {
            self.quick_menu = false;
            changed = true;
        }
        if self.format_open {
            self.format_open = false;
            changed = true;
        }
        if self.pinned_expanded {
            self.pinned_expanded = false;
            changed = true;
        }
        if self.receipt_open.take().is_some() {
            changed = true;
        }
        if self.msg_search_popup.take().is_some() {
            changed = true;
        }
        if self.msg_search_presets_open {
            self.msg_search_presets_open = false;
            changed = true;
        }
        if self.partial_quote_fab.take().is_some() {
            changed = true;
        }
        changed
    }

    /// Collects `(uid, display_name)` pairs for members of a group so `@mention` metadata
    /// carries real Zalo UIDs.
    pub(crate) fn group_member_uid_names(&self, conv: &ChatConv) -> Vec<(String, String)> {
        let mut seen = HashSet::<String>::new();
        let mut out = Vec::new();
        if let Some(prof) = self.profile_ref(&conv.thread_id) {
            for uid in &prof.member_ids {
                if uid.is_empty() || !seen.insert(uid.clone()) {
                    continue;
                }
                if let Some(name) = self
                    .profile_ref(uid)
                    .map(|p| p.name.trim().to_owned())
                    .filter(|n| !n.is_empty())
                {
                    out.push((uid.clone(), name));
                }
            }
        }
        if let Some(msgs) = self.messages.get(&conv.id) {
            for msg in msgs {
                if msg.sender_uid.is_empty()
                    || msg.sender_uid == "0"
                    || msg.sender.trim().is_empty()
                    || !seen.insert(msg.sender_uid.clone())
                {
                    continue;
                }
                out.push((msg.sender_uid.clone(), msg.sender.trim().to_owned()));
            }
        }
        if let Some(view) = self.members_view.as_ref().filter(|v| v.conv == conv.id) {
            for (uid, name) in &view.list {
                if !uid.is_empty() && !name.trim().is_empty() && seen.insert(uid.clone()) {
                    out.push((uid.clone(), name.trim().to_owned()));
                }
            }
        }
        out
    }

    /// Recalls one or several sent messages in a conversation using `/api/message/multi_undo`
    /// or `/api/group/multi_undomsg` when more than one message is selected.
    pub(crate) fn recall_selected_msgs(&mut self, msgs: &[ChatMsg]) {
        let recallable: Vec<&ChatMsg> = msgs
            .iter()
            .filter(|m| m.from_me && !m.recalled)
            .collect();
        if recallable.is_empty() {
            self.set_note("Chỉ có thể thu hồi tin nhắn do bạn gửi");
            return;
        }
        let conv_id = recallable[0].conv;
        let Some(conv) = self.conv(conv_id).cloned() else {
            return;
        };
        let recalled_text = str_vi("STR_UNDO_MESSAGE", "Tin nhắn đã thu hồi").to_owned();
        let recall_ids: HashSet<u64> = recallable.iter().map(|m| m.id).collect();
        if let Some(list) = self.messages.get_mut(&conv_id) {
            for m in list.iter_mut() {
                if recall_ids.contains(&m.id) {
                    m.recalled = true;
                    m.text = recalled_text.clone();
                    m.kind = crate::media::MsgKind::Text;
                    m.media_url.clear();
                    m.media_path = None;
                    m.href.clear();
                    m.group_layout_id = 0;
                    if conv.live {
                        data::persist_recall_message(
                            &conv.thread_id,
                            m.global_msg_id,
                            m.cli_msg_id,
                            &recalled_text,
                        );
                    }
                }
            }
        }
        if let Some(c) = self.convs.iter_mut().find(|c| c.id == conv_id) {
            c.preview = recalled_text;
        }
        if conv.live {
            let base = (data::now_ms().max(1) as u64).saturating_mul(10);
            let items: Vec<zalo_client::MultiUndoItem> = recallable
                .iter()
                .enumerate()
                .filter_map(|(i, m)| {
                    Some(zalo_client::MultiUndoItem {
                        cli_msg_id: base.saturating_add(i as u64),
                        cli_msg_id_undo: m.cli_msg_id?,
                        global_msg_id: m.global_msg_id?,
                    })
                })
                .collect();
            if items.len() == 1 {
                let it = &items[0];
                self.maybe_live(LiveAction::Undo {
                    thread_id: conv.thread_id.clone(),
                    is_group: conv.is_group,
                    msg_id: it.global_msg_id,
                    client_id: it.cli_msg_id,
                    cli_msg_id_undo: it.cli_msg_id_undo,
                });
            } else if items.len() > 1 {
                self.maybe_live(LiveAction::UndoMulti {
                    thread_id: conv.thread_id.clone(),
                    is_group: conv.is_group,
                    items,
                });
            }
        }
        self.set_note(format!("Đã thu hồi {} tin nhắn", recallable.len()));
    }

    fn quote_target(&self, _conv: &ChatConv) -> Option<zalo_client::QuoteTarget> {
        let reply = self.reply_to.as_ref()?;
        let global = reply.global_msg_id?;
        let cli = reply.cli_msg_id?;
        Some(zalo_client::QuoteTarget {
            from_uid: if reply.from_me || reply.sender_uid == "0" {
                self.self_uid()?.to_owned()
            } else {
                reply.sender_uid.clone()
            },
            msg_id: global,
            cli_msg_id: cli,
            cli_msg_type: zalo_client::MSG_TEXT,
            ts: reply.at_ms,
            qmsg: reply.text.clone(),
        })
    }

    pub(crate) fn push_message(&mut self, msg: ChatMsg) {
        let conv_id = msg.conv;
        if let Some(conv) = self.convs.iter_mut().find(|row| row.id == conv_id) {
            conv.preview = if msg.text.trim() == "/-strong" {
                "👍".to_owned()
            } else {
                msg.text.clone()
            };
            conv.time = if msg.at_ms > 0 {
                data::vn_hm(msg.at_ms)
            } else {
                msg.time.clone()
            };
            conv.unread = 0;
        }
        self.messages.entry(conv_id).or_default().push(msg);
        if self.selected == Some(conv_id) {
            self.chat_scrolled_up = false;
            self.clear_open_unread_and_mark_read(conv_id);
        }
        self.scroll_msgs_to_bottom();
    }

    pub(crate) fn maybe_live(&self, action: LiveAction) {
        let allowed = match &action {
            LiveAction::Block { uid } => data::live_allowed(uid),
            LiveAction::Send { thread_id, .. }
            | LiveAction::SendContact { thread_id, .. }
            | LiveAction::Pin { thread_id, .. }
            | LiveAction::Mute { thread_id, .. }
            | LiveAction::Archive { thread_id, .. }
            | LiveAction::Undo { thread_id, .. }
            | LiveAction::UndoMulti { thread_id, .. }
            | LiveAction::Delete { thread_id, .. }
            | LiveAction::React { thread_id, .. }
            | LiveAction::Forward {
                dest_id: thread_id, ..
            }
            | LiveAction::SendPhoto { thread_id, .. }
            | LiveAction::SendPhotoByUrl { thread_id, .. }
            | LiveAction::SendFile { thread_id, .. }
            | LiveAction::SendSticker { thread_id, .. }
            | LiveAction::SendGif { thread_id, .. }
            | LiveAction::SendVoice { thread_id, .. }
            | LiveAction::SendVideo { thread_id, .. }
            | LiveAction::Seen { thread_id, .. }
            | LiveAction::Delivered { thread_id, .. }
            | LiveAction::Typing { thread_id, .. }
            | LiveAction::RemoveUnread { thread_id, .. }
            | LiveAction::PinBoard { thread_id, .. }
            | LiveAction::UnpinBoard { thread_id, .. } => data::live_allowed(thread_id),
            LiveAction::MarkUnread { item } | LiveAction::DeleteConv { item, .. } => {
                data::live_allowed(&item.thread_id)
            }
            LiveAction::ForwardMulti { targets, .. } => {
                !targets.is_empty() && targets.iter().all(|(tid, _)| data::live_allowed(tid))
            }
            LiveAction::RemoveFriend { uid }
            | LiveAction::UpdateAlias { uid, .. }
            | LiveAction::ReportAbuse { target_id: uid, .. } => data::live_allowed(uid),
            LiveAction::VotePoll { .. } => true,
        };
        if !self.has_session || !allowed {
            return;
        }
        if let Some(dir) = self.session_dir.clone() {
            bridge::spawn_live(action, dir);
        }
    }

    /// Rail / composer "Screenshot", using the mode chosen on the chevron.
    pub(crate) fn capture_screenshot(
        &mut self,
        window: Option<gpui::AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        self.capture_screenshot_mode(!self.shot_with_zalo, window, cx);
    }

    /// Region screenshot, optionally minimizing the Zalo window first (`slot == 1`).
    ///
    /// On X11, captures the active monitor natively via `x11rb` + RandR and opens
    /// Zalo's built-in fullscreen region-crop overlay (`screenshot_ui`) with zero
    /// external CLI dependencies. On Wayland/Flatpak, delegates to Electron-parity
    /// desktop region tools and `org.freedesktop.portal.Screenshot` (`interactive=true`).
    pub(crate) fn capture_screenshot_mode(
        &mut self,
        hide_window: bool,
        window: Option<gpui::AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        self.sticker_open = false;
        self.rail_popup = None;
        self.menu = None;
        self.note = None;
        if self.capture_busy {
            return;
        }
        self.capture_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            // Let the dropdown menu / toast repaint away first.
            gpui::Timer::after(std::time::Duration::from_millis(60)).await;
            if hide_window {
                // Must run outside `&mut ShellView` borrow; otherwise `wh.update`
                // fails re-entrantly and silently skips minimizing the Zalo window.
                if let Some(wh) = window {
                    let _ = cx.update(|cx| {
                        let _ = wh.update(cx, |_, w, _| w.minimize_window());
                    });
                }
                cx.background_executor()
                    .spawn(async move { crate::x11_frame::wait_app_windows_hidden(500) })
                    .await;
            } else {
                gpui::Timer::after(std::time::Duration::from_millis(40)).await;
            }

            if crate::screenshot_ui::native_x11_available() {
                let grabbed = cx
                    .background_executor()
                    .spawn(async move { crate::screenshot_ui::grab_active_monitor() })
                    .await;
                let _ = cx.update(|cx| match grabbed {
                    Ok(captured) => {
                        let this_done = this.clone();
                        let open_res = crate::screenshot_ui::open_screenshot_overlay(
                            captured,
                            move |outcome, cx| {
                                let _ = this_done.update(cx, |view, cx| {
                                    view.capture_busy = false;
                                    match outcome {
                                        crate::screenshot_ui::ShotOutcome::Send {
                                            path,
                                            png_bytes,
                                        } => {
                                            crate::clip::copy_image(
                                                cx,
                                                gpui::ImageFormat::Png,
                                                png_bytes,
                                            );
                                            view.set_pending_photo(path);
                                            view.set_note(
                                                "Đã chụp — ảnh đã chép vào clipboard. Nhấn Enter để gửi",
                                            );
                                        }
                                        crate::screenshot_ui::ShotOutcome::Copied { png_bytes } => {
                                            crate::clip::copy_image(
                                                cx,
                                                gpui::ImageFormat::Png,
                                                png_bytes,
                                            );
                                            view.set_note("Đã sao chép ảnh chụp vào clipboard");
                                        }
                                        crate::screenshot_ui::ShotOutcome::Saved {
                                            path,
                                            png_bytes,
                                        } => {
                                            crate::clip::copy_image(
                                                cx,
                                                gpui::ImageFormat::Png,
                                                png_bytes,
                                            );
                                            let name = path
                                                .file_name()
                                                .and_then(|n| n.to_str())
                                                .unwrap_or("ZaloScreenshot.png");
                                            view.set_note(format!("Đã lưu ảnh chụp: {name}"));
                                        }
                                        crate::screenshot_ui::ShotOutcome::Cancelled => {
                                            view.note = None;
                                        }
                                    }
                                    cx.notify();
                                });
                                if let Some(wh) = window {
                                    let _ = wh.update(cx, |_, w, _| w.activate_window());
                                }
                            },
                            cx,
                        );
                        if let Err(err) = open_res {
                            let _ = this.update(cx, |view, cx| {
                                view.capture_busy = false;
                                view.set_note(format!("Chụp màn hình: {err}"));
                                cx.notify();
                            });
                            if hide_window {
                                if let Some(wh) = window {
                                    let _ = wh.update(cx, |_, w, _| w.activate_window());
                                }
                            }
                        }
                    }
                    Err(err) => {
                        let _ = this.update(cx, |view, cx| {
                            view.capture_busy = false;
                            view.set_note(format!("Chụp màn hình: {err}"));
                            cx.notify();
                        });
                        if hide_window {
                            if let Some(wh) = window {
                                let _ = wh.update(cx, |_, w, _| w.activate_window());
                            }
                        }
                    }
                });
                return;
            }

            let result = cx
                .background_executor()
                .spawn(async move { crate::plugins::capture_region() })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.capture_busy = false;
                match result {
                    Ok(Some(path)) => {
                        if let Ok(bytes) = std::fs::read(&path) {
                            crate::clip::copy_image(cx, gpui::ImageFormat::Png, bytes);
                        }
                        view.set_pending_photo(path);
                        view.set_note("Đã chụp — ảnh đã chép vào clipboard. Nhấn Enter để gửi");
                    }
                    Ok(None) => view.note = None,
                    Err(err) => view.set_note(format!("Chụp màn hình: {err}")),
                }
                cx.notify();
            });
            if hide_window {
                if let Some(wh) = window {
                    let _ = wh.update(cx, |_, w, _| w.activate_window());
                }
            }
        })
        .detach();
    }

    /// Append one photo to the composer tray. Enter sends the whole tray.
    pub(crate) fn set_pending_photo(&mut self, path: PathBuf) {
        self.add_pending_photos(std::iter::once(path));
    }

    /// Append picked photos. Electron's tray holds several; 50 matches a full row set.
    pub(crate) fn add_pending_photos(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        const MAX: usize = 50;
        for path in paths {
            if self.pending_photos.len() >= MAX {
                self.set_note("Đã đủ 50 ảnh");
                break;
            }
            let thumb = crate::media::decode_thumb(&path, 160);
            self.pending_photos.push(path);
            self.pending_thumbs.push(thumb);
            self.pending_captions.push(String::new());
        }
    }

    pub(crate) fn clear_pending_photo(&mut self) {
        self.pending_photos.clear();
        self.pending_thumbs.clear();
        self.pending_captions.clear();
        self.caption_edit = None;
    }

    /// Drop one tray photo. The trash button on that thumb calls this.
    pub(crate) fn remove_pending_photo(&mut self, index: usize) {
        if index >= self.pending_photos.len() {
            return;
        }
        self.pending_photos.remove(index);
        if index < self.pending_thumbs.len() {
            self.pending_thumbs.remove(index);
        }
        if index < self.pending_captions.len() {
            self.pending_captions.remove(index);
        }
        self.caption_edit = match self.caption_edit {
            Some(cur) if self.pending_photos.is_empty() || cur == index => None,
            Some(cur) if cur > index => Some(cur - 1),
            other => other,
        };
    }

    /// Pencil on a thumb: edit that photo's caption, not the shared composer.
    pub(crate) fn begin_caption_edit(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.pending_photos.len() {
            return;
        }
        self.commit_caption_edit(cx);
        while self.pending_captions.len() < self.pending_photos.len() {
            self.pending_captions.push(String::new());
        }
        let text = self.pending_captions.get(index).cloned().unwrap_or_default();
        self.caption_edit = Some(index);
        self.caption_input
            .update(cx, |input, cx| input.set_text(text, cx));
        let handle = gpui::Focusable::focus_handle(self.caption_input.read(cx), cx);
        window.focus(&handle);
    }

    pub(crate) fn commit_caption_edit(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.caption_edit else {
            return;
        };
        if index >= self.pending_captions.len() {
            return;
        }
        self.pending_captions[index] = self.caption_input.read(cx).text();
    }

    /// Upload a local file to the open conversation (asyncfile flow).
    pub(crate) fn send_file_now(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_owned();
        let Some(conv) = self.selected_conv() else {
            self.set_note("Chọn một hội thoại để gửi");
            return;
        };
        let now = data::now_ms();
        let id = self.next_msg_id;
        self.next_msg_id += 1;
        let mut msg = ChatMsg::text_msg(
            id,
            conv.id,
            conv.thread_id.clone(),
            true,
            str_vi("STR_YOU", "Bạn").to_owned(),
            self.self_uid().unwrap_or_default().to_owned(),
            format!("[File] {name}"),
            data::vn_hm(now),
            now,
            Some(now),
            None,
        );
        msg.kind = crate::media::MsgKind::File;
        msg.file_name = name.clone();
        msg.file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        msg.media_path = Some(path.clone());
        self.push_message(msg);
        if conv.live && self.has_session {
            self.maybe_live(LiveAction::SendFile {
                client_id: now as u64,
                thread_id: conv.thread_id,
                is_group: conv.is_group,
                path,
            });
            self.set_note(format!("Đang gửi «{name}» lên máy chủ…"));
        } else {
            self.set_note("Hội thoại mẫu / chưa đăng nhập — file chỉ hiện trên máy này");
        }
    }

    /// Local video bubble, then chunk upload + `video_done` + forward.
    pub(crate) fn send_video_now(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video.mp4")
            .to_owned();
        let Some(conv) = self.selected_conv() else {
            self.set_note("Chọn một hội thoại để gửi");
            return;
        };
        let probed = crate::video::inspect(&path);
        let now = data::now_ms();
        let id = self.next_msg_id;
        self.next_msg_id += 1;
        let mut msg = ChatMsg::text_msg(
            id,
            conv.id,
            conv.thread_id.clone(),
            true,
            str_vi("STR_YOU", "Bạn").to_owned(),
            self.self_uid().unwrap_or_default().to_owned(),
            "[Video]".to_owned(),
            data::vn_hm(now),
            now,
            Some(now),
            None,
        );
        msg.kind = crate::media::MsgKind::Video;
        msg.file_name = name.clone();
        msg.href = path.display().to_string();
        msg.media_w = probed.width;
        msg.media_h = probed.height;
        msg.file_size = probed.duration_ms;
        msg.media_path = probed.thumb.clone();
        self.push_message(msg);
        self.warm_media(conv.id);
        if conv.live && self.has_session {
            self.maybe_live(LiveAction::SendVideo {
                client_id: now as u64,
                thread_id: conv.thread_id,
                is_group: conv.is_group,
                path,
                duration_ms: probed.duration_ms,
                width: probed.width,
                height: probed.height,
                thumb: probed.thumb,
            });
            self.set_note(format!("Đang gửi video «{name}»…"));
        } else {
            self.set_note("Hội thoại mẫu / chưa đăng nhập — video chỉ hiện trên máy này");
        }
    }

    /// Local bubble + real upload (`upload_photo_best` → `send_photo`).
    pub(crate) fn send_photo_now(&mut self, path: PathBuf, caption: &str) {
        self.send_photo_now_at(path, caption, 0, 0, 1, 0);
    }

    /// Forward/share an existing CDN photo by URL (`/api/message/photo_url` or `/api/group/photo_url`)
    /// without re-uploading bytes, matching Zalo PC `sendPhotoByUrl`.
    pub(crate) fn send_photo_by_url_now(
        &mut self,
        dest: &ChatConv,
        src: &ChatMsg,
        stagger_ms: i64,
    ) {
        let now = data::now_ms().saturating_add(stagger_ms);
        let id = self.next_msg_id;
        self.next_msg_id += 1;
        let caption = if src.text == "[Hình ảnh]" {
            String::new()
        } else {
            src.text.clone()
        };
        let mut msg = ChatMsg::text_msg(
            id,
            dest.id,
            dest.thread_id.clone(),
            true,
            str_vi("STR_YOU", "Bạn").to_owned(),
            self.self_uid().unwrap_or_default().to_owned(),
            if caption.is_empty() {
                "[Hình ảnh]".to_owned()
            } else {
                caption.clone()
            },
            data::vn_hm(now),
            now,
            Some(now),
            None,
        );
        msg.kind = crate::media::MsgKind::Photo;
        msg.media_path = src.media_path.clone();
        msg.media_url = src.media_url.clone();
        msg.media_w = src.media_w;
        msg.media_h = src.media_h;
        self.push_message(msg);
        self.warm_media(dest.id);
        if dest.live && self.has_session {
            let info = zalo_client::PhotoUrlSendInfo {
                title: caption,
                ori_url: src.media_url.clone(),
                thumb_url: src.media_url.clone(),
                hd_url: src.media_url.clone(),
                width: src.media_w,
                height: src.media_h,
                properties: "{}".to_owned(),
            };
            self.maybe_live(LiveAction::SendPhotoByUrl {
                client_id: now as u64,
                thread_id: dest.thread_id.clone(),
                is_group: dest.is_group,
                info,
            });
            self.set_note("Đang chia sẻ ảnh…");
        }
    }

    /// `stagger_ms` keeps `client_id` unique when several photos leave in one tick.
    /// `layout_id` is Electron's `groupLayoutId` for a multi-photo send (`0` for one photo).
    fn send_photo_now_at(
        &mut self,
        path: PathBuf,
        caption: &str,
        stagger_ms: i64,
        layout_id: i64,
        group_total: u32,
        group_index: u32,
    ) {
        let Some(conv) = self.selected_conv() else {
            self.set_note("Chọn một hội thoại để gửi");
            return;
        };
        let now = data::now_ms().saturating_add(stagger_ms);
        let id = self.next_msg_id;
        self.next_msg_id += 1;
        let (w, h) = image::image_dimensions(&path).unwrap_or((0, 0));
        let mut msg = ChatMsg::text_msg(
            id,
            conv.id,
            conv.thread_id.clone(),
            true,
            str_vi("STR_YOU", "Bạn").to_owned(),
            self.self_uid().unwrap_or_default().to_owned(),
            if caption.is_empty() {
                "[Hình ảnh]".to_owned()
            } else {
                caption.to_owned()
            },
            data::vn_hm(now),
            now,
            Some(now),
            None,
        );
        msg.kind = crate::media::MsgKind::Photo;
        msg.media_path = Some(path.clone());
        msg.media_w = w;
        msg.media_h = h;
        if group_total > 1 && layout_id != 0 {
            msg.group_layout_id = layout_id;
        }
        let sent_layout = msg.group_layout_id;
        self.push_message(msg);
        self.warm_media(conv.id);
        if conv.live && self.has_session {
            self.maybe_live(LiveAction::SendPhoto {
                client_id: now as u64,
                thread_id: conv.thread_id,
                is_group: conv.is_group,
                path,
                caption: caption.to_owned(),
                group_layout_id: sent_layout,
                group_total,
                group_index,
            });
            self.set_note("Đang gửi ảnh lên máy chủ…");
        } else {
            self.set_note("Hội thoại mẫu / chưa đăng nhập — ảnh chỉ hiện trên máy này");
        }
    }

    pub(crate) fn pick_and_attach(&mut self, photo: bool) {
        self.sticker_open = false;
        if photo {
            match crate::plugins::pick_photos() {
                Ok(paths) if !paths.is_empty() => self.add_pending_photos(paths),
                Ok(_) => {}
                Err(err) => self.set_note(err),
            }
            return;
        }
        match crate::plugins::pick_file() {
            Ok(Some(path)) if crate::video::is_video_path(&path) => self.send_video_now(path),
            Ok(Some(path)) => self.send_file_now(path),
            Ok(None) => {}
            Err(err) => self.set_note(err),
        }
    }

    /// Handles external files/images dropped onto the chat pane (`drag-media-overlay`):
    /// stages images into the pending photo tray and sends dropped videos/documents.
    pub(crate) fn handle_dropped_paths(&mut self, paths: Vec<PathBuf>) {
        if self.selected.is_none() || paths.is_empty() {
            return;
        }
        let (images, videos, files, skipped_dirs) = classify_dropped_paths(paths);
        let mut handled = 0usize;
        if !images.is_empty() {
            handled += images.len();
            self.add_pending_photos(images);
        }
        for v in videos {
            handled += 1;
            self.send_video_now(v);
        }
        for f in files {
            handled += 1;
            self.send_file_now(f);
        }
        if handled == 0 && skipped_dirs > 0 {
            self.set_note("Chưa hỗ trợ kéo thả thư mục — hãy chọn tập tin hoặc hình ảnh");
        }
    }
}

/// Splits dropped OS paths into `(images, videos, other_files, skipped_dirs)`.
pub(crate) fn classify_dropped_paths(
    paths: Vec<PathBuf>,
) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>, usize) {
    let mut images = Vec::new();
    let mut videos = Vec::new();
    let mut files = Vec::new();
    let mut skipped_dirs = 0usize;
    for p in paths {
        if p.is_dir() {
            skipped_dirs += 1;
            continue;
        }
        if !p.is_file() {
            continue;
        }
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "jxl" | "avif" | "heic" | "heif"
        ) {
            images.push(p);
        } else if crate::video::is_video_path(&p) {
            videos.push(p);
        } else {
            files.push(p);
        }
    }
    (images, videos, files, skipped_dirs)
}

impl ShellView {
    /// True when the right-hand chat / contacts page should be visible.
    /// In `Compact` mode (`<= 880 px`), only one of Sidebar or Chat is shown at once.
    pub(crate) fn compact_shows_chat(&self) -> bool {
        self.compact_level != CompactLevel::Compact
            || (self.compact_show_chat && !self.starred_open)
    }

    /// True when the conversation list / contacts nav sidebar should be visible.
    pub(crate) fn compact_shows_sidebar(&self) -> bool {
        self.compact_level != CompactLevel::Compact || !self.compact_shows_chat()
    }

    /// Dock the info column when it fits. Otherwise it floats and a click
    /// outside closes it. Widening restores the saved preference.
    /// Also updates Zalo PC's 3-level responsive mode (`Normal` / `SemiCompact` / `Compact`)
    /// and the exact live `chat_pane_w` budget for message layout.
    fn sync_info_layout(&mut self, window: &Window) {
        let csd = if crate::window_frame::CsdCorners::from_window(window).any_pad() {
            zalo_design::WINDOW_SHADOW * 2.0
        } else {
            0.0
        };
        let width = (f32::from(window.viewport_size().width) - csd).max(0.0);
        let next_level = CompactLevel::for_width(width, self.solo.is_some());
        if self.compact_level != CompactLevel::Compact && next_level == CompactLevel::Compact {
            self.compact_show_chat = self.selected.is_some() && !self.starred_open;
        }
        self.compact_level = next_level;
        let wide = crate::info::info_docks(width, self.solo.is_some());
        let became_narrow = self.info_wide == Some(true) && !wide;
        self.info_wide = Some(wide);
        if wide {
            self.info_open = self.info_pref && self.selected.is_some();
            self.info_overlay = false;
        } else {
            if became_narrow || self.selected.is_none() {
                self.info_open = false;
            }
            self.info_overlay = self.info_open;
        }
        let left_w = if self.solo.is_some() {
            0.0
        } else if next_level == CompactLevel::Compact {
            // In Compact mode the 344 px conversation list hides when chat is shown,
            // leaving only the 64 px left rail (`#sidebarNav.compact-mode.show-chat`).
            zalo_design::MAIN_TAB_W
        } else {
            zalo_design::SIDEBAR_W
        };
        let search_docks = self.msg_search_open
            && self.selected.is_some()
            && next_level == CompactLevel::Normal;
        let right_docked_w = if search_docks {
            crate::info::INFO_PANEL_W
        } else if self.info_open && !self.info_overlay && self.rail != Rail::Contacts {
            crate::info::INFO_PANEL_W
        } else {
            0.0
        };
        self.chat_pane_w = (width - left_w - right_docked_w).max(200.0);
    }

    /// Title-bar pill: Light / Dark / System, same values as Settings › Appearance.
    fn open_theme_menu(
        &mut self,
        event: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_menu(
            crate::context_menu::MenuKind::Theme,
            event.position(),
            window,
        );
        cx.notify();
    }

    /// Info column: in-flow when the window is wide, floating when it is not.
    fn info_slot(
        &self,
        selected: Option<&crate::data::ChatConv>,
        corners: crate::window_frame::CsdCorners,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let panel = self.info_panel(selected, corners, cx).into_any_element();
        if !self.info_overlay {
            return panel;
        }
        div()
            .id("info-overlay")
            .absolute()
            .top(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .w(px(crate::info::INFO_PANEL_W))
            .occlude()
            .shadow_lg()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(panel)
            .into_any_element()
    }

    /// Full-row catcher under the floating info column. A docked column has
    /// no catcher: chat clicks must not close it.
    fn info_dismiss(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("info-dismiss")
            .absolute()
            .inset(px(0.))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.info_open = false;
                    this.info_overlay = false;
                    cx.notify();
                }),
            )
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _prof = RenderProf::start();
        self.persist_window_state(window);
        if window.focused(cx).is_none() {
            window.focus(&self.root_focus);
        }
        let corners = CsdCorners::from_window(window);
        let note = self.rt_status.clone();
        let toast = self.note.clone();
        self.arm_note_timer(cx);
        self.apply_notice_custom();
        if self.solo.is_none() {
            bind_notice_wake(cx.weak_entity());
        }
        if self.pending_notice.is_some() {
            cx.defer_in(window, |this, window, cx| this.flush_notice(window, cx));
        }
        if self.kick_to_login {
            self.kick_to_login = false;
            cx.defer_in(window, |this, window, cx| {
                this.leave_to_login(window, cx, false);
            });
        }
        let menu = self.menu.is_some();
        let forward = self.forward_of.is_some();
        let selected = self.selected_conv();
        self.sync_info_layout(window);
        if self.prefs.bool("followSystem", false) {
            let dark = matches!(
                window.appearance(),
                gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
            );
            if dark != crate::color::is_dark() {
                crate::color::set_dark(dark);
            }
        }
        let info = self.info_open;
        let warm_key = self.selected.map(|id| {
            (
                id,
                self.messages_of(id).len(),
                self.msg_window,
                info,
                self.composer_link.clone(),
            )
        });
        let warm_changed = self.last_warm_key != warm_key;
        if warm_changed {
            self.last_warm_key = warm_key;
            self.warm_links(cx);
            self.warm_polls(cx);
        }
        self.publish_badge();
        let shares = crate::photo_window::take_shares();
        if !shares.is_empty() {
            window.activate_window();
            for msg in shares {
                self.open_share(msg, cx);
            }
        }
        if info && warm_changed {
            if let Some(id) = self.selected {
                self.warm_info_media(id);
                self.warm_group_pending(id, cx);
                if self.conv(id).is_some_and(|c| c.is_group) {
                    self.warm_e2ee_groups(cx);
                }
            }
        }
        if self.rail_popup == Some(crate::rail::RailPopup::Catalog) {
            self.warm_catalog(cx);
        }
        if let Some(id) = self.selected {
            self.fetch_message_status(id, false, cx);
            self.fetch_friend_board_pins(id, cx);
        }
        self.drain_media_queue(cx);
        self.drain_sticker_thumbs(cx);
        self.drain_labels(cx);
        // The friend list is not in the conversation keys, so its photos never
        // download until every cached profile is queued.
        if self.rail == Rail::Contacts && !self.avatar_all {
            self.avatar_all = true;
            self.avatars_queued = true;
        }
        self.drain_profiles(cx);
        self.fill_profile_gaps(cx);
        self.drain_presence(cx);
        self.drain_media_store(cx);
        self.drain_info_join(cx);
        self.start_realtime(window, cx);
        self.start_shortcuts(window, cx);
        self.ensure_privacy_loaded(cx);
        self.sync_open_seen(window.is_window_active());
        let hint = match self.selected_conv() {
            Some(row) if self.english => format!("Type @, message to {}", row.name),
            Some(row) => format!("Nhập @, tin nhắn tới {}", row.name),
            None => str_vi("STR_SEND_MESSAGE", "Nhập tin nhắn").to_owned(),
        };
        let mention_names = self.mention_known_names();
        self.composer.update(cx, |input, _| {
            input.set_placeholder(hint);
            input.set_mention_names(mention_names);
        });
        let contacts_hint = if self.contact_tab == crate::sidebar::ContactTab::Groups {
            str_vi("STR_SEARCH_GROUP", "Tìm nhóm theo tên").to_owned()
        } else {
            crate::i18n::t("Tìm bạn")
        };
        self.contacts_query
            .update(cx, |input, _| input.set_placeholder(contacts_hint));
        let search_hint = crate::i18n::t("Tìm kiếm");
        self.search
            .update(cx, |input, _| input.set_placeholder(search_hint));
        let solo = self.solo.is_some();
        let me_name = self
            .profiles
            .map
            .get(crate::profiles::ME)
            .map(|p| p.name.clone());
        let main_title = match self
            .display_name
            .clone()
            .or(me_name)
            .filter(|n| !n.is_empty())
        {
            Some(name) => format!("Zalo - {name}"),
            None => "Zalo".to_owned(),
        };
        if self.last_window_title != main_title {
            self.last_window_title = main_title.clone();
            window.set_window_title(&main_title);
        }
        let root_bg = token_rgb("--layer-background", "#ffffff");
        let root_ink = token_rgb("--text-primary", "#081b3a");
        let solo_title = selected
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Zalo".into());
        if solo {
            return wrap_csd(
                window,
                corners
                    .apply(
                        div()
                            .id("shell")
                            .track_focus(&self.root_focus)
                            .on_key_down(cx.listener(ShellView::on_shell_key))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    if this.dismiss_floating_panels() {
                                        cx.notify();
                                    }
                                }),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(|this, _, _, cx| {
                                    if this.dismiss_floating_panels() {
                                        cx.notify();
                                    }
                                }),
                            )
                            .relative()
                            .flex()
                            .flex_col()
                            .size_full()
                            .overflow_hidden()
                            .bg(rgb(root_bg)),
                        true,
                        true,
                        true,
                        true,
                    )
                    .text_color(rgb(root_ink))
                    .text_size(px(zalo_design::FONT_BASE))
                    .font_family(crate::fonts::FAMILY_UI)
                    .child(main_titlebar_with_theme(
                        corners,
                        window,
                        solo_title,
                        false,
                        self.english,
                        self.update_info.as_ref().map(|u| u.tag.as_str()),
                        cx.listener(|this, _, window, cx| {
                            let en = !this.english;
                            this.english = en;
                            crate::i18n::set_english(en);
                            this.set_pref(
                                "lang",
                                serde_json::Value::String(if en { "en" } else { "vi" }.into()),
                                window,
                                cx,
                            );
                            cx.notify();
                        }),
                        cx.listener(ShellView::open_theme_menu),
                        cx.listener(|this, _, _, cx| {
                            if this.update_info.is_some() {
                                this.dialog = Some(crate::dialogs::Dialog::UpdateAvailable);
                            } else {
                                this.dialog = Some(crate::dialogs::Dialog::About);
                            }
                            cx.notify();
                        }),
                    ))
                    .when_some(note, |el, text| el.child(self.session_banner(text, cx)))
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_row()
                            .flex_1()
                            .min_h(px(0.))
                            .child(self.chat_column(corners, cx))
                            .when(self.info_overlay, |el| el.child(self.info_dismiss(cx)))
                            .when(info, |el| {
                                el.child(self.info_slot(selected.as_ref(), corners, cx))
                            }),
                    )
                    .when(self.dialog.is_some(), |el| {
                        el.child(self.dialog_overlay(cx))
                    })
                    .when(self.viewer.is_some(), |el| {
                        el.child(self.viewer_overlay(window, cx))
                    })
                    .when(menu, |el| el.child(self.context_menu_overlay(cx)))
                    .when(self.react_picker.is_some(), |el| {
                        el.child(self.react_picker_overlay(cx))
                    })
                    .when(self.react_detail.is_some(), |el| {
                        el.child(self.react_detail_overlay(cx))
                    })
                    .when(self.partial_quote_fab.is_some(), |el| {
                        el.child(self.partial_quote_fab_overlay(window, cx))
                    })
                    .when(self.like_hold.is_some(), |el| {
                        el.child(self.like_hold_overlay(window))
                    })
                    .when(!self.react_bursts.is_empty(), |el| {
                        el.child(self.react_burst_overlay())
                    })
                    .when(forward, |el| el.child(self.forward_overlay(cx)))
                    .when_some(toast, |el, text| el.child(toast_el(text)))
                    .when(self.locked, |el| el.child(self.lock_overlay(cx))),
            );
        }
        wrap_csd(
            window,
            corners
                .apply(
                    div()
                        .id("shell")
                        .track_focus(&self.root_focus)
                        .on_key_down(cx.listener(ShellView::on_shell_key))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                if this.dismiss_floating_panels() {
                                    cx.notify();
                                }
                            }),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Right,
                            cx.listener(|this, _, _, cx| {
                                if this.dismiss_floating_panels() {
                                    cx.notify();
                                }
                            }),
                        )
                        .relative()
                        .flex()
                        .flex_row()
                        .size_full()
                        .overflow_hidden()
                        .bg(gpui::transparent_black()),
                    true,
                    true,
                    true,
                    true,
                )
                .text_color(rgb(root_ink))
                .text_size(px(zalo_design::FONT_BASE))
                .font_family(crate::fonts::FAMILY_UI)
                .child(self.rail_column(corners, cx))
                .child(
                    corners
                        .apply(
                            div()
                                .relative()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .h_full()
                                .min_w(px(0.))
                                .bg(rgb(root_bg)),
                            false,
                            true,
                            false,
                            true,
                        )
                        .child(main_titlebar_with_theme(
                            corners,
                            window,
                            main_title,
                            true,
                            self.english,
                            self.update_info.as_ref().map(|u| u.tag.as_str()),
                            cx.listener(|this, _, window, cx| {
                                let en = !this.english;
                                this.english = en;
                                crate::i18n::set_english(en);
                                this.set_pref(
                                    "lang",
                                    serde_json::Value::String(if en { "en" } else { "vi" }.into()),
                                    window,
                                    cx,
                                );
                                cx.notify();
                            }),
                            cx.listener(|this, _, window, cx| {
                                let next = if crate::color::is_dark() {
                                    "light"
                                } else {
                                    "dark"
                                };
                                this.set_pref(
                                    "theme",
                                    serde_json::Value::String(next.into()),
                                    window,
                                    cx,
                                );
                                cx.notify();
                            }),
                            cx.listener(|this, _, _, cx| {
                                if this.update_info.is_some() {
                                    this.dialog = Some(crate::dialogs::Dialog::UpdateAvailable);
                                } else {
                                    this.dialog = Some(crate::dialogs::Dialog::About);
                                }
                                cx.notify();
                            }),
                        ))
                        .when_some(note, |el, text| el.child(self.session_banner(text, cx)))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_1()
                                .w_full()
                                .min_h(px(0.))
                                .relative()
                                .when(self.compact_shows_sidebar(), |el| {
                                    el.child(self.sidebar_column(cx))
                                })
                                .when(self.compact_shows_chat(), |el| {
                                    el.child(self.chat_column(corners, cx))
                                })
                                .when(self.info_overlay && !self.msg_search_open, |el| {
                                    el.child(self.info_dismiss(cx))
                                })
                                .when(self.msg_search_open && selected.is_some(), |el| {
                                    let panel = self.msg_search_panel(selected.as_ref(), corners, cx);
                                    if self.compact_level == CompactLevel::Normal {
                                        el.child(panel)
                                    } else {
                                        el.child(
                                            div()
                                                .id("msg-search-overlay")
                                                .absolute()
                                                .top_0()
                                                .right_0()
                                                .bottom_0()
                                                .w(px(crate::info::INFO_PANEL_W
                                                    .min(self.chat_pane_w)))
                                                .occlude()
                                                .shadow_lg()
                                                .child(panel),
                                        )
                                    }
                                })
                                .when(
                                    info && self.rail != Rail::Contacts && !self.msg_search_open,
                                    |el| el.child(self.info_slot(selected.as_ref(), corners, cx)),
                                ),
                        ),
                )
                .when(self.settings_open, |el| {
                    let vh = f32::from(window.viewport_size().height);
                    let h = (vh - 64.0).clamp(480.0, 668.0);
                    el.child(self.settings_overlay(corners, h, cx))
                })
                .when(self.rail_popup.is_some(), |el| {
                    el.child(self.rail_popup_overlay(cx))
                })
                .when(self.dialog.is_some(), |el| {
                    el.child(self.dialog_overlay(cx))
                })
                .when(self.viewer.is_some(), |el| {
                    el.child(self.viewer_overlay(window, cx))
                })
                .when(menu, |el| el.child(self.context_menu_overlay(cx)))
                .when(self.react_picker.is_some(), |el| {
                    el.child(self.react_picker_overlay(cx))
                })
                .when(self.react_detail.is_some(), |el| {
                    el.child(self.react_detail_overlay(cx))
                })
                .when(self.partial_quote_fab.is_some(), |el| {
                    el.child(self.partial_quote_fab_overlay(window, cx))
                })
                .when(self.like_hold.is_some(), |el| {
                    el.child(self.like_hold_overlay(window))
                })
                .when(!self.react_bursts.is_empty(), |el| {
                    el.child(self.react_burst_overlay())
                })
                .when(forward, |el| el.child(self.forward_overlay(cx)))
                .when_some(toast, |el, text| el.child(toast_el(text)))
                .when(self.export_ui.popup_open(), |el| el.child(self.transfer_popup(cx)))
                .when(self.backup_ui.popup_open(), |el| {
                    el.child(self.backup_popup_overlay(cx))
                })
                .when(self.locked, |el| el.child(self.lock_overlay(cx))),
        )
    }
}

/// `ZALO_GPUI_PROF=1`: print how long building the element tree took.
struct RenderProf(Option<std::time::Instant>);

impl RenderProf {
    fn start() -> Self {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let on = *ON.get_or_init(|| std::env::var_os("ZALO_GPUI_PROF").is_some());
        Self(on.then(std::time::Instant::now))
    }
}

impl Drop for RenderProf {
    fn drop(&mut self) {
        if let Some(t) = self.0 {
            eprintln!(
                "zalo-gpui render {:.2} ms",
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

/// Zalo toast: rounded pill near the bottom centre, legible in both themes.
fn toast_el(text: String) -> impl IntoElement {
    let pill = token_rgb("--layer-background-inverse-subtle", "#1d2025");
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(96.))
        .flex()
        .justify_center()
        .child(
            div()
                .id("shell-toast")
                .occlude()
                .max_w(px(480.))
                .px(px(16.))
                .py(px(10.))
                .rounded(px(8.))
                .bg(rgb(pill))
                .border_1()
                .border_color(crate::color::border_subtle())
                .shadow_md()
                .text_color(rgb(0xffffff))
                .text_size(px(14.))
                .child(crate::i18n::t(&text)),
        )
}

impl ShellView {
    fn session_banner(&self, text: String, cx: &mut Context<Self>) -> gpui::AnyElement {
        let expired = text.contains("hết hạn") || text.contains("Đăng nhập lại");
        let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
        let ink = token_rgb("--text-primary", "#081b3a");
        let blue = token_rgb("--B55", "#0068ff");
        if !expired {
            return note_bar(text).into_any_element();
        }
        div()
            .id("session-banner")
            .flex()
            .flex_none()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(32.))
            .px(px(16.))
            .bg(rgb(field))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(12.))
                    .text_color(rgb(ink))
                    .truncate()
                    .child(text),
            )
            .child(
                div()
                    .id("session-relogin")
                    .h(px(24.))
                    .px(px(10.))
                    .rounded(px(4.))
                    .bg(rgb(blue))
                    .text_color(rgb(crate::color::on_accent_fg()))
                    .text_size(px(12.))
                    .font_family(crate::fonts::FAMILY_SEMIBOLD)
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.leave_to_login(window, cx, false);
                    }))
                    .child("Đăng nhập lại"),
            )
            .into_any_element()
    }
}

fn note_bar(text: impl Into<SharedString>) -> impl IntoElement {
    let field = token_rgb("--input-field-bg-filled", "#f1f2f4");
    let ink = token_rgb("--text-primary", "#081b3a");
    div()
        .id("shell-note")
        .flex()
        .flex_none()
        .items_center()
        .h(px(28.))
        .px(px(16.))
        .bg(rgb(field))
        .text_size(px(12.))
        .text_color(rgb(ink))
        .child(text.into())
}

/// True when the binary should open the main shell instead of login.
pub fn preview_shell_requested() -> bool {
    std::env::var_os("ZALO_GPUI_PREVIEW").is_some() || data::session_present()
}

/// Default windowed size.
pub fn main_window_size() -> (f32, f32) {
    MAIN_WINDOW_DEFAULT
}

/// `MIN_WIDTH_WINDOW_APP` 580 × 400.
pub fn main_window_min() -> (f32, f32) {
    (
        zalo_design::css_layout::JS_MIN_WIDTH_WINDOW_APP,
        MAIN_WINDOW_MIN_H,
    )
}

pub(crate) fn can_open_new_thread(
    has_session: bool,
    thread_id: &str,
    is_group: bool,
    mine: &HashSet<String>,
) -> bool {
    let tid = thread_id.trim();
    has_session && data::live_allowed(tid) && (is_group || !mine.contains(tid))
}

pub(crate) fn resolve_thread_title(
    thread_id: &str,
    name: &str,
    profiles: &HashMap<String, crate::profiles::Profile>,
) -> String {
    let trimmed = name.trim();
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }
    profiles
        .get(thread_id)
        .map(|p| p.name.trim().to_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| thread_id.to_owned())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn open_thread_into(
    convs: &mut Vec<ChatConv>,
    messages: &mut HashMap<u64, Vec<ChatMsg>>,
    profiles: &HashMap<String, crate::profiles::Profile>,
    thread_id: &str,
    name: &str,
    is_group: bool,
    has_session: bool,
    mine: &HashSet<String>,
) -> Option<u64> {
    let raw_tid = thread_id.trim();
    let normalized_tid = if is_group && !raw_tid.is_empty() && !raw_tid.starts_with("preview:") {
        let clean = raw_tid
            .strip_prefix("g_")
            .or_else(|| {
                raw_tid
                    .strip_prefix('g')
                    .filter(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
            })
            .unwrap_or(raw_tid);
        format!("g_{clean}")
    } else {
        raw_tid.to_owned()
    };
    let tid = normalized_tid.as_str();
    let existing = if !tid.is_empty() {
        convs.iter().find(|c| c.thread_id == tid).map(|c| c.id)
    } else {
        let trimmed = name.trim();
        (!trimmed.is_empty())
            .then(|| convs.iter().find(|c| c.name == trimmed).map(|c| c.id))
            .flatten()
    };
    if let Some(id) = existing {
        return Some(id);
    }
    if !can_open_new_thread(has_session, tid, is_group, mine) {
        return None;
    }
    let mut next_id = data::hash_id(tid);
    while convs.iter().any(|c| c.id == next_id) {
        next_id = next_id.wrapping_add(1);
        if next_id <= 4 {
            next_id = 5;
        }
    }
    let display = resolve_thread_title(tid, name, profiles);
    convs.insert(
        0,
        ChatConv {
            id: next_id,
            thread_id: tid.to_owned(),
            name: display,
            preview: String::new(),
            time: String::new(),
            unread: 0,
            is_group,
            online: false,
            live: true,
        },
    );
    messages.entry(next_id).or_default();
    Some(next_id)
}

/// getgi error 164 stores this placeholder when the account has left the group.
/// Those rows are not conversations Electron keeps in the list.
#[must_use]
pub(crate) fn is_left_group_name(name: &str) -> bool {
    name.trim() == "Nhóm đã rời"
}

/// Shared `ListFilter` predicate used by `ShellView::listed_convs` and unit tests.
pub(crate) fn conv_matches_list_filter(
    filter: ListFilter,
    query: &str,
    stranger: bool,
    unread: u32,
    label: Option<i64>,
) -> bool {
    let _ = query;
    match filter {
        // Electron's list for this account has no "Tin nhắn từ người lạ" folder.
        // Those threads are ordinary rows, so a right-click can delete them.
        ListFilter::All => true,
        ListFilter::Unread => unread > 0,
        ListFilter::Strangers => stranger,
        ListFilter::Label(id) => label == Some(id),
    }
}

/// "My Cloud" (Electron labels the same thread My Documents).
pub(crate) fn is_my_documents(name: &str, thread_id: &str, send2me_id: Option<&str>) -> bool {
    matches!(name, "Cloud của tôi" | "My Documents" | "My Cloud")
        || send2me_id.is_some_and(|id| !id.is_empty() && id == thread_id)
}

/// Pure stranger predicate using loaded `get_friends` flags in `profiles`.
pub(crate) fn is_stranger_conv(
    conv: &ChatConv,
    has_session: bool,
    profiles: &HashMap<String, crate::profiles::Profile>,
    send2me_id: Option<&str>,
    uin: Option<&str>,
) -> bool {
    if !has_session || !conv.live || conv.is_group {
        return false;
    }
    // Only fold strangers once `get_friends` has populated at least one `friend: true` entry.
    if !profiles.values().any(|p| p.friend) {
        return false;
    }
    if is_my_documents(&conv.name, &conv.thread_id, send2me_id)
        || uin.is_some_and(|id| id == conv.thread_id)
        || conv.name == "Zalo"
    {
        return false;
    }
    !profiles.get(&conv.thread_id).is_some_and(|p| p.friend)
}

/// `starredMsgs` in `gpui-prefs.json`: SQLite `message_id` values.
fn parse_starred_ids(raw: &str) -> HashSet<u64> {
    serde_json::from_str::<Vec<u64>>(raw)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn starred_ids_json(set: &HashSet<u64>) -> serde_json::Value {
    let mut ids: Vec<u64> = set.iter().copied().collect();
    ids.sort_unstable();
    serde_json::json!(ids)
}

/// Matches `@All` / `@Tất cả` and `@<member_name>` spans inside a group message
/// and builds Zalo's UTF-16 `MentionItem` descriptors for `/api/group/mention`.
pub(crate) fn build_group_mentions(
    text: &str,
    members: &[(String, String)],
) -> Vec<zalo_client::MentionItem> {
    if !text.contains('@') {
        return Vec::new();
    }
    let mut candidates: Vec<(&str, &str, i64)> = Vec::with_capacity(members.len() + 2);
    candidates.push(("-1", "All", 1));
    candidates.push(("-1", "Tất cả", 1));
    for (uid, name) in members {
        let trimmed = name.trim();
        if !uid.is_empty() && !trimmed.is_empty() {
            candidates.push((uid.as_str(), trimmed, 0));
        }
    }
    // Longest name first so `@Trường Nguyễn` wins over `@Trường`.
    candidates.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let mut out = Vec::new();
    let mut byte_idx = 0usize;
    let bytes = text.as_bytes();
    while byte_idx < bytes.len() {
        let Some(rel) = text[byte_idx..].find('@') else {
            break;
        };
        let at_pos = byte_idx + rel;
        let prev_ok = at_pos == 0
            || text[..at_pos]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_alphanumeric() && c != '_');
        if !prev_ok {
            byte_idx = at_pos + 1;
            continue;
        }
        let after = &text[at_pos + 1..];
        let matched = candidates.iter().find_map(|&(uid, name, kind)| {
            let rest = after.strip_prefix(name)?;
            let next_ok = rest.is_empty()
                || rest
                    .chars()
                    .next()
                    .is_some_and(|c| !c.is_alphanumeric() && c != '_');
            next_ok.then_some((uid, name, kind))
        });
        if let Some((uid, name, kind)) = matched {
            let span_end = at_pos + 1 + name.len();
            let pos = text[..at_pos].encode_utf16().count();
            let len = text[at_pos..span_end].encode_utf16().count();
            out.push(zalo_client::MentionItem {
                pos,
                uid: uid.to_owned(),
                len,
                kind,
            });
            byte_idx = span_end;
        } else {
            byte_idx = at_pos + 1;
        }
    }
    out
}

/// Parses `/api/social/profile/getmute` into normalized thread ids (`uid` / `g_<grid>`).
pub(crate) fn muted_threads_from(value: &serde_json::Value) -> Vec<String> {
    let root = value.get("data").unwrap_or(value);
    let now_s = data::now_ms() / 1000;
    let mut out = Vec::new();
    let mut collect = |key: &str, group: bool| {
        let Some(entries) = root.get(key) else {
            return;
        };
        let list: Vec<&serde_json::Value> = if let Some(arr) = entries.as_array() {
            arr.iter().collect()
        } else if let Some(obj) = entries.as_object() {
            obj.values().collect()
        } else {
            Vec::new()
        };
        for item in list {
            let Some(id) = item
                .get("id")
                .or_else(|| item.get("chatId"))
                .or_else(|| item.get("uid"))
                .and_then(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .or_else(|| v.as_i64().map(|n| n.to_string()))
                })
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            // Skip expired mute timers (`muteDuration > 0 && endTime < now`).
            let expired = item
                .get("endTime")
                .or_else(|| item.get("muteUntil"))
                .and_then(serde_json::Value::as_i64)
                .is_some_and(|t| t > 0 && t < now_s);
            if expired {
                continue;
            }
            if group && !id.starts_with("g_") {
                out.push(format!("g_{}", id.trim_start_matches('g')));
            } else {
                out.push(id);
            }
        }
    };
    collect("chatEntries", false);
    collect("groupChatEntries", true);
    out
}

/// Parses `/api/conv/getUnreadMark` into normalized thread ids (`uid` / `g_<grid>`).
pub(crate) fn unread_mark_threads_from(value: &serde_json::Value) -> HashSet<String> {
    let root = value.get("data").unwrap_or(value);
    let mut out = HashSet::new();
    let mut collect = |key: &str, group: bool| {
        let Some(arr) = root.get(key).and_then(serde_json::Value::as_array) else {
            return;
        };
        for item in arr {
            let raw = item
                .as_str()
                .map(str::to_owned)
                .or_else(|| {
                    item.get("id").and_then(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .or_else(|| v.as_i64().map(|n| n.to_string()))
                    })
                })
                .unwrap_or_default();
            if raw.is_empty() {
                continue;
            }
            if group {
                out.insert(format!(
                    "g_{}",
                    raw.trim_start_matches("g_").trim_start_matches('g')
                ));
            } else {
                out.insert(raw);
            }
        }
    };
    collect("convsUser", false);
    collect("convsGroup", true);
    out
}

/// Parses `/api/preloadconvers/get-last-seen` into `(normalized_thread_id, global_msg_id)` pairs.
pub(crate) fn last_seen_from(value: &serde_json::Value) -> Vec<(String, i64)> {
    let root = value.get("data").unwrap_or(value);
    let mut out = Vec::new();
    let mut collect = |key: &str, group: bool| {
        let Some(node) = root.get(key) else {
            return;
        };
        if let Some(arr) = node.as_array() {
            for item in arr {
                let raw_id = item
                    .get("id")
                    .or_else(|| item.get("groupId"))
                    .or_else(|| item.get("uid"))
                    .and_then(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .or_else(|| v.as_i64().map(|n| n.to_string()))
                    })
                    .unwrap_or_default();
                let msg_id = item
                    .get("mId")
                    .or_else(|| item.get("globalMsgId"))
                    .or_else(|| item.get("lastSeenMsgId"))
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                    })
                    .unwrap_or(0);
                if !raw_id.is_empty() && msg_id > 0 {
                    let tid = if group && !raw_id.starts_with("g_") {
                        format!("g_{}", raw_id.trim_start_matches('g'))
                    } else {
                        raw_id
                    };
                    out.push((tid, msg_id));
                }
            }
        } else if let Some(map) = node.as_object() {
            for (k, v) in map {
                let msg_id = v
                    .as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                    .or_else(|| {
                        v.get("mId")
                            .or_else(|| v.get("globalMsgId"))
                            .and_then(|x| {
                                x.as_i64()
                                    .or_else(|| x.as_str().and_then(|s| s.parse::<i64>().ok()))
                            })
                    })
                    .unwrap_or(0);
                if msg_id > 0 {
                    let tid = if group && !k.starts_with("g_") {
                        format!("g_{}", k.trim_start_matches('g'))
                    } else {
                        k.clone()
                    };
                    out.push((tid, msg_id));
                }
            }
        }
    };
    collect("oneone", false);
    collect("group", true);
    out
}

/// Extracts group cloud messages from `/api/cm/rgetv2` or `/api/cm/mget` responses.
fn cloud_group_items_from(
    value: &serde_json::Value,
    thread_id: &str,
    self_uid: &str,
) -> Vec<crate::realtime::Incoming> {
    let root = value.get("data").unwrap_or(value);
    let mut raw_msgs: Vec<&serde_json::Value> = Vec::new();
    if let Some(arr) = root.as_array() {
        raw_msgs.extend(arr.iter());
    } else if let Some(arr) = root.get("msgs").and_then(serde_json::Value::as_array) {
        raw_msgs.extend(arr.iter());
    } else if let Some(arr) = root.get("groupMsgs").and_then(serde_json::Value::as_array) {
        raw_msgs.extend(arr.iter());
    } else if let Some(arr) = root.get("messages").and_then(serde_json::Value::as_array) {
        raw_msgs.extend(arr.iter());
    } else if let Some(arr) = root.get("path").and_then(serde_json::Value::as_array) {
        raw_msgs.extend(arr.iter());
    } else if let Some(obj) = root.as_object() {
        for v in obj.values() {
            if let Some(arr) = v.as_array() {
                raw_msgs.extend(arr.iter());
            } else if let Some(arr) = v.get("msgs").and_then(serde_json::Value::as_array) {
                raw_msgs.extend(arr.iter());
            } else if let Some(arr) = v.get("groupMsgs").and_then(serde_json::Value::as_array) {
                raw_msgs.extend(arr.iter());
            }
        }
    }
    let mut out = Vec::new();
    for item in raw_msgs {
        if let Some(mut inc) = crate::realtime::parse_item(item, true, self_uid) {
            if inc.thread_id.is_empty() || inc.thread_id == "g_" {
                inc.thread_id = thread_id.to_owned();
            }
            out.push(inc);
        }
    }
    out
}

/// Merges a server echo / sync [`crate::realtime::Incoming`] into an existing
/// local [`ChatMsg`] row without ever leaving `MsgKind::Link` with an empty
/// `href` or clobbering richer local text (`@All https://...`).
///
/// Returns `true` when media URLs changed and cached bitmaps should be refreshed.
fn merge_incoming_into_row(row: &mut ChatMsg, m: &crate::realtime::Incoming) -> bool {
    let mut upgraded_media = false;
    if m.kind != crate::media::MsgKind::Text && row.kind == crate::media::MsgKind::Text {
        row.kind = m.kind;
        let resolved_href = if !m.href.is_empty() {
            m.href.clone()
        } else if m.kind == crate::media::MsgKind::Link {
            crate::media::first_link(&m.text)
                .or_else(|| crate::media::first_link(&row.text))
                .unwrap_or_default()
        } else {
            String::new()
        };
        // Preserve richer local text (e.g. "@All https://lza.lol") when
        // the server link echo only carries the bare URL.
        let keep_local_text = m.kind == crate::media::MsgKind::Link
            && !row.text.trim().is_empty()
            && !resolved_href.is_empty()
            && m.text.trim() == resolved_href.trim()
            && row.text.contains(&resolved_href)
            && row.text.trim() != resolved_href.trim();
        if !keep_local_text && !m.text.trim().is_empty() {
            row.text = m.text.clone();
        }
        if !m.file_name.is_empty() {
            row.file_name = m.file_name.clone();
        }
        if m.kind == crate::media::MsgKind::Link {
            if !resolved_href.is_empty() {
                row.href = resolved_href;
            }
        } else {
            if !m.media_url.is_empty() {
                row.media_url = m.media_url.clone();
                row.media_path =
                    crate::media::cache_path_for(&row.media_url).filter(|p| p.is_file());
                upgraded_media = true;
            }
            if !resolved_href.is_empty() {
                row.href = resolved_href;
            }
        }
        if m.width > 0 {
            row.media_w = m.width;
        }
        if m.height > 0 {
            row.media_h = m.height;
        }
    } else if row.kind == crate::media::MsgKind::Photo
        && m.kind == crate::media::MsgKind::Photo
        && !m.media_url.is_empty()
        && row.media_url != m.media_url
        && !crate::media::is_thumb_photo_url(&m.media_url)
    {
        row.media_url = m.media_url.clone();
        row.media_path = crate::media::cache_path_for(&row.media_url).filter(|p| p.is_file());
        if m.width > 0 {
            row.media_w = m.width;
        }
        if m.height > 0 {
            row.media_h = m.height;
        }
        if crate::media::is_local_media_uri(&row.text) {
            row.text = m.text.clone();
        }
        upgraded_media = true;
    } else if row.kind == crate::media::MsgKind::Text
        && (m.text == "Tin nhắn đã bị xóa"
            || m.text == "[Tin nhắn đã tự xóa]"
            || m.text.starts_with("[Việc cần làm]"))
        && row.text != m.text
    {
        row.text = m.text.clone();
    } else if row.href.is_empty() && !m.href.is_empty() {
        row.href = m.href.clone();
    }
    if row.kind == crate::media::MsgKind::Link && row.href.is_empty() {
        if let Some(url) = crate::media::first_link(&m.text)
            .or_else(|| crate::media::first_link(&row.text))
        {
            row.href = url;
        }
    }
    upgraded_media
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn my_documents_matches_cloud_thread() {
        assert!(is_my_documents("Cloud của tôi", "1", None));
        assert!(is_my_documents("Lan", "999", Some("999")));
        assert!(!is_my_documents("Lan", "1", Some("999")));
        assert!(!is_my_documents("Zalo", "1", None));
    }

    #[test]
    fn starred_ids_round_trip() {
        let mut set = HashSet::new();
        set.insert(9);
        set.insert(2);
        let raw = starred_ids_json(&set).to_string();
        assert_eq!(raw, "[2,9]");
        assert_eq!(parse_starred_ids(&raw), set);
        assert!(parse_starred_ids("null").is_empty());
        assert!(parse_starred_ids("").is_empty());
    }

    #[test]
    fn shell_adds_up() {
        assert_eq!(
            zalo_design::MAIN_TAB_W + zalo_design::CONV_LIST_INNER_W,
            zalo_design::SIDEBAR_W
        );
        assert_eq!(zalo_design::LINUX_TITLEBAR_H, 38.0);
        assert_eq!(main_window_min().0, 580.0);
        assert_eq!(main_window_min().1, 400.0);
        assert_eq!(main_window_size(), (1200.0, 720.0));
        assert!(!preview::DEFAULT_SPLIT);
        assert!(!crate::plugins::AUTO_TRAY);
    }

    #[test]
    fn clear_unreads_and_seens_sync_state() {
        let mut rs = crate::read_sync::ReadSync::default();
        let rec = serde_json::json!({
            "cmi": "10", "gmi": "200", "si": "555", "di": "0",
            "mt": "webchat", "st": 0, "at": 0, "cmd": 0, "ts": "1794400000000"
        });
        rs.note_incoming("555", 1_794_400_000_000, &rec);
        assert!(rs.seen_due("555").is_some());
        let status = zalo_client::StatusUpdate {
            read: vec![zalo_client::ReadMark {
                thread_id: "555".into(),
                last_msg_id: Some(200),
                ts: Some(1_794_400_000_000),
            }],
            seen: vec![zalo_client::SeenMark {
                thread_id: "555".into(),
                msg_id: 250,
            }],
            ..zalo_client::StatusUpdate::default()
        };
        let cleared = rs.apply_status(&status, None, |m| m.ts);
        assert_eq!(cleared.len(), 1);
        assert!(rs.seen_due("555").is_none());
        assert!(rs.peer_saw("555", Some(240), 100, |_| None));
        assert!(!rs.peer_saw("555", Some(260), 100, |_| None));
    }

    #[test]
    fn reaction_updates_and_normalize_thread_ids() {
        assert_eq!(
            crate::read_sync::normalize_thread_id("u12345").as_deref(),
            Some("12345")
        );
        assert_eq!(
            crate::read_sync::normalize_thread_id("g98765").as_deref(),
            Some("g_98765")
        );
        assert_eq!(crate::read_sync::normalize_thread_id("m1"), None);

        let mut map = HashMap::new();
        crate::read_sync::update_reaction(&mut map, 42, "100", "/-heart", 5, false);
        crate::read_sync::update_reaction(&mut map, 42, "101", "/-strong", 3, false);
        assert_eq!(map.get(&42).map(Vec::len), Some(2));
        crate::read_sync::update_reaction(&mut map, 42, "100", "", -1, true);
        assert_eq!(map.get(&42).map(Vec::len), Some(1));
        assert_eq!(
            crate::read_sync::reaction_emoji(&map[&42][0].r_icon, map[&42][0].r_type),
            "👍"
        );
        crate::read_sync::update_reaction(&mut map, 42, "101", "", -1, true);
        assert!(!map.contains_key(&42));
    }

    #[test]
    fn open_thread_and_kick_permissions() {
        let mut convs = Vec::new();
        let mut messages = HashMap::new();
        let mut profiles = HashMap::new();
        profiles.insert(
            "100000000000000004".into(),
            crate::profiles::Profile {
                name: "Minh".into(),
                ..crate::profiles::Profile::default()
            },
        );
        let mine: HashSet<String> = ["99999999".into()].into_iter().collect();

        // Offline/preview/self rejected for new threads.
        assert_eq!(
            open_thread_into(
                &mut convs,
                &mut messages,
                &profiles,
                "100000000000000004",
                "",
                false,
                false,
                &mine
            ),
            None
        );
        assert_eq!(
            open_thread_into(
                &mut convs,
                &mut messages,
                &profiles,
                "preview:1",
                "Preview",
                false,
                true,
                &mine
            ),
            None
        );
        assert_eq!(
            open_thread_into(
                &mut convs,
                &mut messages,
                &profiles,
                "99999999",
                "Me",
                false,
                true,
                &mine
            ),
            None
        );

        // Real peer thread gets created once and reused on second open.
        let id = open_thread_into(
            &mut convs,
            &mut messages,
            &profiles,
            "100000000000000004",
            "",
            false,
            true,
            &mine,
        )
        .unwrap();
        let expected_id = data::hash_id("100000000000000004");
        assert_eq!(id, expected_id);
        assert_eq!(convs[0].name, "Minh");
        assert!(convs[0].live);
        assert_eq!(
            open_thread_into(
                &mut convs,
                &mut messages,
                &profiles,
                "100000000000000004",
                "Minh",
                false,
                true,
                &mine,
            ),
            Some(expected_id)
        );
        assert_eq!(convs.len(), 1);
        // Group id without `g_` prefix normalizes and matches existing `g_` group.
        let gid = open_thread_into(
            &mut convs,
            &mut messages,
            &profiles,
            "200000000000000002",
            "Team",
            true,
            true,
            &mine,
        )
        .unwrap();
        assert_eq!(gid, data::hash_id("g_200000000000000002"));
        assert_eq!(
            open_thread_into(
                &mut convs,
                &mut messages,
                &profiles,
                "g_200000000000000002",
                "Team",
                true,
                true,
                &mine,
            ),
            Some(gid)
        );
        assert_eq!(convs.len(), 2);

        // Kick permissions: creator can kick admins/members; admin only regular members.
        let admins = vec!["200".into()];
        let creator_mine: HashSet<String> = ["100".into()].into_iter().collect();
        assert!(!crate::info::can_kick_member(
            &creator_mine,
            "100",
            &admins,
            "100"
        ));
        assert!(crate::info::can_kick_member(
            &creator_mine,
            "100",
            &admins,
            "200"
        ));
        assert!(crate::info::can_kick_member(
            &creator_mine,
            "100",
            &admins,
            "300"
        ));
        let admin_mine: HashSet<String> = ["200".into()].into_iter().collect();
        assert!(!crate::info::can_kick_member(
            &admin_mine,
            "100",
            &admins,
            "100"
        ));
        assert!(!crate::info::can_kick_member(
            &admin_mine,
            "100",
            &admins,
            "200"
        ));
        assert!(crate::info::can_kick_member(
            &admin_mine,
            "100",
            &admins,
            "300"
        ));
        let member_mine: HashSet<String> = ["300".into()].into_iter().collect();
        assert!(!crate::info::can_kick_member(
            &member_mine,
            "100",
            &admins,
            "400"
        ));
    }

    #[test]
    fn stranger_detection_requires_friend_cache_and_excludes_self_cloud_groups_and_friends() {
        let make_conv =
            |id: u64, thread_id: &str, name: &str, is_group: bool, live: bool| ChatConv {
                id,
                thread_id: thread_id.into(),
                name: name.into(),
                preview: String::new(),
                time: "10:00".into(),
                unread: 1,
                is_group,
                online: false,
                live,
            };
        let mut profiles = HashMap::new();
        let stranger = make_conv(10, "555", "Người lạ A", false, true);
        let friend = make_conv(11, "222", "Bạn B", false, true);
        let group = make_conv(12, "g_333", "Nhóm C", true, true);
        let cloud = make_conv(13, "999", "Cloud của tôi", false, true);
        let me = make_conv(14, "111", "Tôi", false, true);
        let zalo_oa = make_conv(15, "000", "Zalo", false, true);
        let preview = make_conv(1, "preview:1", "Preview", false, false);

        // No session or no loaded `friend: true` cache -> never flag anyone.
        assert!(!is_stranger_conv(
            &stranger,
            false,
            &profiles,
            Some("999"),
            Some("111")
        ));
        assert!(!is_stranger_conv(
            &stranger,
            true,
            &profiles,
            Some("999"),
            Some("111")
        ));

        profiles.insert(
            "222".into(),
            crate::profiles::Profile {
                name: "Bạn B".into(),
                friend: true,
                ..Default::default()
            },
        );
        profiles.insert(
            "555".into(),
            crate::profiles::Profile {
                name: "Người lạ A".into(),
                friend: false,
                ..Default::default()
            },
        );

        assert!(is_stranger_conv(
            &stranger,
            true,
            &profiles,
            Some("999"),
            Some("111")
        ));
        assert!(!is_stranger_conv(
            &friend,
            true,
            &profiles,
            Some("999"),
            Some("111")
        ));
        assert!(!is_stranger_conv(
            &group,
            true,
            &profiles,
            Some("999"),
            Some("111")
        ));
        assert!(!is_stranger_conv(
            &cloud,
            true,
            &profiles,
            Some("999"),
            Some("111")
        ));
        assert!(!is_stranger_conv(
            &me,
            true,
            &profiles,
            Some("999"),
            Some("111")
        ));
        assert!(!is_stranger_conv(
            &zalo_oa,
            true,
            &profiles,
            Some("999"),
            Some("111")
        ));
        assert!(!is_stranger_conv(
            &preview,
            true,
            &profiles,
            Some("999"),
            Some("111")
        ));

        // Strangers stay in the default list. Electron has no stranger folder row.
        assert!(conv_matches_list_filter(
            ListFilter::All,
            "",
            true,
            2,
            None
        ));
        assert!(is_left_group_name("Nhóm đã rời"));
        assert!(!is_left_group_name("Trường Group"));
        assert!(conv_matches_list_filter(
            ListFilter::All,
            "người",
            true,
            2,
            None
        ));
        assert!(conv_matches_list_filter(
            ListFilter::All,
            "",
            false,
            0,
            None
        ));
        assert!(conv_matches_list_filter(
            ListFilter::Strangers,
            "",
            true,
            0,
            None
        ));
        assert!(!conv_matches_list_filter(
            ListFilter::Strangers,
            "",
            false,
            3,
            None
        ));
        assert!(conv_matches_list_filter(
            ListFilter::Unread,
            "",
            true,
            2,
            None
        ));
    }

    #[test]
    fn p0_group_mentions_and_server_state_parsers() {
        let members = vec![
            ("101".to_owned(), "Trường".to_owned()),
            ("102".to_owned(), "Trường Nguyễn".to_owned()),
        ];
        let mentions = build_group_mentions("Chào @Trường Nguyễn và @All nhé", &members);
        assert_eq!(mentions.len(), 2);
        assert_eq!(mentions[0].uid, "102");
        assert_eq!(mentions[0].kind, 0);
        assert_eq!(mentions[0].pos, "Chào ".encode_utf16().count());
        assert_eq!(mentions[0].len, "@Trường Nguyễn".encode_utf16().count());
        assert_eq!(mentions[1].uid, "-1");
        assert_eq!(mentions[1].kind, 1);

        let muted = muted_threads_from(&serde_json::json!({
            "data": {
                "chatEntries": [{"id": "555", "endTime": -1}],
                "groupChatEntries": [{"id": "888", "endTime": -1}]
            }
        }));
        assert!(muted.contains(&"555".to_owned()));
        assert!(muted.contains(&"g_888".to_owned()));

        let unread = unread_mark_threads_from(&serde_json::json!({
            "data": {
                "convsUser": [{"id": "111"}],
                "convsGroup": [{"id": "222"}]
            }
        }));
        assert!(unread.contains("111"));
        assert!(unread.contains("g_222"));

        let seen = last_seen_from(&serde_json::json!({
            "data": {
                "oneone": [{"id": "111", "mId": "9001"}],
                "group": {"222": {"mId": 9002}}
            }
        }));
        assert!(seen.contains(&("111".to_owned(), 9001)));
        assert!(seen.contains(&("g_222".to_owned(), 9002)));
    }

    #[test]
    fn link_echo_merge_preserves_text_and_never_leaves_empty_href() {
        // Case 1: Full Zalo PC `recommened.link` echo matching local optimistic message
        let mut local = ChatMsg::text_msg(
            1,
            10,
            "g_4263295552360969008".into(),
            true,
            "Bạn".into(),
            "1488852095078916019".into(),
            "@All https://lza.lol".into(),
            "18:05".into(),
            1_795_604_752_138,
            Some(1_795_604_752_138),
            None,
        );
        let echo_json = serde_json::json!({
            "uidFrom": "0",
            "idTo": "4263295552360969008",
            "cliMsgId": "1795604752138",
            "msgId": "8528321999999",
            "msgType": "chat.recommended",
            "content": {
                "action": "recommened.link",
                "href": "https://lza.lol",
                "title": "@All https://lza.lol",
                "description": "",
                "thumb": ""
            }
        });
        let incoming =
            crate::realtime::parse_item(&echo_json, true, "1488852095078916019").unwrap();
        merge_incoming_into_row(&mut local, &incoming);
        assert_eq!(local.kind, crate::media::MsgKind::Link);
        assert_eq!(local.href, "https://lza.lol");
        assert_eq!(local.text, "@All https://lza.lol");

        // Case 2: Server echo carries ONLY bare URL while local optimistic row has "@All https://lza.lol"
        let mut local2 = ChatMsg::text_msg(
            2,
            10,
            "g_4263295552360969008".into(),
            true,
            "Bạn".into(),
            "1488852095078916019".into(),
            "@All https://lza.lol".into(),
            "18:05".into(),
            1_795_604_752_138,
            Some(1_795_604_752_138),
            None,
        );
        let bare_echo = crate::realtime::Incoming {
            kind: crate::media::MsgKind::Link,
            text: "https://lza.lol".into(),
            href: String::new(), // worst-case empty href
            from_me: true,
            ..crate::realtime::Incoming::default()
        };
        merge_incoming_into_row(&mut local2, &bare_echo);
        assert_eq!(local2.kind, crate::media::MsgKind::Link);
        assert_eq!(local2.href, "https://lza.lol");
        assert_eq!(local2.text, "@All https://lza.lol");
    }

    #[test]
    fn compact_level_breakpoints_match_zalo_pc() {
        assert_eq!(CompactLevel::for_width(1440.0, false), CompactLevel::Normal);
        assert_eq!(CompactLevel::for_width(1225.0, false), CompactLevel::Normal);
        assert_eq!(
            CompactLevel::for_width(1224.0, false),
            CompactLevel::SemiCompact
        );
        assert_eq!(
            CompactLevel::for_width(960.0, false),
            CompactLevel::SemiCompact
        );
        assert_eq!(
            CompactLevel::for_width(881.0, false),
            CompactLevel::SemiCompact
        );
        assert_eq!(CompactLevel::for_width(880.0, false), CompactLevel::Compact);
        assert_eq!(CompactLevel::for_width(580.0, false), CompactLevel::Compact);
        assert_eq!(CompactLevel::for_width(580.0, true), CompactLevel::Normal);

        // At minimum window width 580 px in Compact mode, hiding the 344 px sidebar
        // leaves 580 - 64 = 516 px for the chat pane — plenty of room for full bubbles
        // + 4 floating actions beside the card with zero clipping.
        let compact_580 =
            crate::chat::ChatLayoutMetrics::for_chat_width(580.0 - zalo_design::MAIN_TAB_W, false);
        assert!(!compact_580.actions_above);
        assert_eq!(compact_580.chat_pane_w, 516.0);
        assert!(compact_580.content_max_w >= 318.0);
    }

    #[test]
    fn cloud_group_items_from_parses_getpath_and_pc_dl_shapes() {
        let sample = |id: &str, body: &str| {
            serde_json::json!({
                "msgId": id,
                "cliMsgId": id,
                "uidFrom": "1001",
                "idTo": "900",
                "dName": "An",
                "msgType": "webchat",
                "content": body,
                "ts": "1730000000000"
            })
        };
        let via_path = serde_json::json!({
            "data": { "path": [sample("101", "tin cũ từ getpath")] }
        });
        let items = cloud_group_items_from(&via_path, "g_900", "9999");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].thread_id, "g_900");
        assert_eq!(items[0].text, "tin cũ từ getpath");

        let via_gmsgs = serde_json::json!({
            "data": { "groupMsgs": [sample("102", "tin từ pc/dl")] }
        });
        let items2 = cloud_group_items_from(&via_gmsgs, "g_900", "9999");
        assert_eq!(items2.len(), 1);
        assert_eq!(items2[0].text, "tin từ pc/dl");
    }
}
