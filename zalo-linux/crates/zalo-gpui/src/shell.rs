//! Main window: `#main-tab` 64 full height + gray titlebar + list 344 + chat.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{
    div, prelude::*, px, rgb, Context, Entity, IntoElement, ParentElement, Render, RenderImage,
    ScrollHandle, SharedString, Styled, Window,
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

/// Avatar URL for `key`, also trying the `g_` form.
fn profile_avatar(profiles: &crate::profiles::Profiles, key: &str) -> Option<String> {
    let url = profiles
        .map
        .get(key)
        .or_else(|| profiles.map.get(&alt_profile_key(key)))
        .map(|p| p.avatar.clone())
        .filter(|u| u.starts_with("https://"));
    url
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
    /// "Pin Message" (local).
    pub(crate) pinned_msgs: HashSet<u64>,
    pub(crate) selected: Option<u64>,
    pub(crate) contact_tab: ContactTab,
    pub(crate) note: Option<String>,
    /// When the current toast was first painted (auto-hide timer armed).
    note_at: Option<std::time::Instant>,
    /// Message popup waiting for the next frame. It opens a screen window.
    pending_notice: Option<crate::notice::NoticeContent>,
    /// Decoded chat wallpaper `(path, image)`.
    pub(crate) wall_image: RefCell<Option<(String, Arc<RenderImage>)>>,
    /// Composer had a selection when its context menu opened.
    pub(crate) input_has_sel: bool,
    /// Info column "Members" page.
    pub(crate) members_view: Option<crate::info::MembersView>,
    /// "Select Multiple Messages": conversation + ticked message ids.
    pub(crate) select_msgs: Option<(u64, Vec<u64>)>,
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
    pub(crate) voice_playing: Option<(u64, std::process::Child)>,
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
    /// Root focus so Esc reaches the shell when no input is focused.
    root_focus: gpui::FocusHandle,
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
    /// Narrow window: the column floats and a click outside closes it.
    pub(crate) info_overlay: bool,
    /// Composer capture includes the Zalo window.
    pub(crate) shot_with_zalo: bool,
    /// Photo-viewer drag origin `(mouse_x, mouse_y, pan_x, pan_y)`.
    pub(crate) viewer_drag: Option<(f32, f32, f32, f32)>,
    /// The pointer moved, so the click must not close the viewer.
    pub(crate) viewer_dragged: bool,
    pub(crate) forward_of: Option<ChatMsg>,
    pub(crate) display_name: Option<String>,
    pub(crate) uin: Option<String>,
    pub(crate) send2me_id: Option<String>,
    pub(crate) session_dir: Option<PathBuf>,
    pub(crate) has_session: bool,
    pub(crate) next_msg_id: u64,
    /// Saved settings (`ui-state.json` + `gpui-prefs.json`).
    pub(crate) prefs: crate::prefs::Prefs,
    /// Last written `window` prefs (w, h, maximized) so render does not rewrite disk.
    window_saved: (i64, i64, bool),
    /// Disk sizes / audio devices for the open settings page.
    pub(crate) settings_cache: Option<crate::settings::SettingsCache>,
    /// Screenshot / picked photo waiting above the composer.
    pub(crate) pending_photo: Option<PathBuf>,
    pub(crate) pending_thumb: Option<Arc<RenderImage>>,
    /// Region picker running.
    pub(crate) capture_busy: bool,
    /// zCloud / Tools / Starred Messages popover next to the rail.
    pub(crate) rail_popup: Option<crate::rail::RailPopup>,
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
    /// `#message-view` scroll; jumps to the newest bubble on open / send.
    pub(crate) msg_scroll: ScrollHandle,
    /// How many of the newest messages the chat builds (grows by 60 on
    /// "Load older messages"). Building all 200+ bubbles every frame lagged.
    pub(crate) msg_window: usize,
    /// Names / avatars / member counts (`profiles.rs`, cached a day).
    pub(crate) profiles: crate::profiles::Profiles,
    /// Decoded avatars keyed like [`crate::profiles::Profiles::map`].
    pub(crate) avatars: HashMap<String, Arc<RenderImage>>,
    pub(crate) profiles_refresh: bool,
    pub(crate) avatars_queued: bool,
    /// One `getprofiles/v2` pass for senders that still have no photo.
    profile_gaps_started: bool,
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
    /// Phone message sync v2 state (`Settings › Sync`).
    pub(crate) phone_sync: crate::phone_sync::PhoneSyncState,
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
    /// Friend-online dots and the open chat's last-visit line.
    pub(crate) presence: crate::presence::Presence,
    /// Info-column media store preview for the open thread.
    pub(crate) media_shelf: crate::media_store::Shelf,
}

/// Newest messages built per open conversation (see `msg_window`).
pub const MSG_WINDOW: usize = 60;

/// Where settings are saved; `None` in unit tests.
fn prefs_dir() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    zalo_core::XdgDirs::from_env()
        .ok()
        .map(|x| x.config_home().join("zalo-rust"))
        .filter(|d| d.is_dir())
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

impl ShellView {
    /// Load `store.db` / `session.json` when present. Tests use preview fixtures.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        window.set_window_title("Zalo");
        cx.observe_window_appearance(window, |_, window, _| {
            window.refresh();
        })
        .detach();
        let search =
            cx.new(|cx| LineInput::new(cx, str_vi("STR_PLACE_HOLDER_SEARCH_PRODUCT", "Tìm kiếm")));
        let composer = cx.new(|cx| LineInput::new(cx, str_vi("STR_SEND_MESSAGE", "Nhập tin nhắn")));
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
        let snap = data::load_snapshot();
        let mut this = Self {
            rail: Rail::Chats,
            list_filter: ListFilter::All,
            other_tab: false,
            labels: Vec::new(),
            labels_state: LabelsState::Idle,
            labels_pending: false,
            conv_labels: HashMap::new(),
            starred: HashSet::new(),
            pinned_msgs: HashSet::new(),
            selected: initial_thread(&snap.convs),
            contact_tab: ContactTab::Friends,
            note: None,
            note_at: None,
            pending_notice: None,
            wall_image: RefCell::new(None),
            input_has_sel: false,
            members_view: None,
            select_msgs: None,
            link_previews: crate::links::load(crate::links::cache_path().as_deref()),
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
            info_overlay: false,
            shot_with_zalo: true,
            viewer_drag: None,
            viewer_dragged: false,
            forward_of: None,
            display_name: snap.display_name,
            uin: snap.uin,
            send2me_id: snap.send2me_id,
            session_dir: snap.session_dir,
            has_session: snap.has_session,
            next_msg_id: snap.next_msg_id,
            prefs: crate::prefs::Prefs::load(prefs_dir()),
            window_saved: (0, 0, false),
            settings_cache: None,
            pending_photo: None,
            pending_thumb: None,
            capture_busy: false,
            rail_popup: None,
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
            msg_scroll: ScrollHandle::new(),
            msg_window: MSG_WINDOW,
            profiles: crate::profiles::Profiles::load(crate::profiles::cache_path().as_deref()),
            avatars: HashMap::new(),
            profiles_refresh: false,
            avatars_queued: true,
            profile_gaps_started: false,
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
            stickers: StickerState {
                recent: crate::stickers::load_recent_stickers(),
                ..StickerState::default()
            },
            sticker_search,
            gif_search,
            voice_rec: None,
            privacy: Default::default(),
            shortcuts: Default::default(),
            phone_sync: Default::default(),
            quick_menu: false,
            format_open: false,
            urgency: 0,
            urgency_msgs: HashMap::new(),
            react_picker: None,
            presence: crate::presence::Presence::default(),
            media_shelf: crate::media_store::Shelf::default(),
        };
        if let Some(saved) = this.prefs.json("receipts") {
            this.read_sync.load_cursors(&saved);
        }
        if let Some(id) = this.selected {
            this.warm_media(id);
            this.msg_scroll.scroll_to_bottom();
        }
        // Older caches have no `friend` flag: refresh once so Contacts is real.
        let flagged = this.profiles.map.values().any(|p| p.friend);
        this.profiles_refresh =
            this.has_session && (this.profiles.stale(data::now_ms()) || !flagged);
        this.wire_lock_input(cx);
        // A set screen-lock code also locks at start-up (zalo-gui behaviour).
        if !this.prefs.str("lockHash", "").is_empty() {
            this.locked = true;
            let handle = gpui::Focusable::focus_handle(this.lock_input.read(cx), cx);
            window.focus(&handle);
        }
        this.load_hidden(cx);
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
            Ok("starred") => Some(crate::rail::RailPopup::Starred),
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
        this
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
                    && msg.media_url.starts_with("https://")
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
                    _ => crate::media::PHOTO_MAX_W as u32,
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
    /// eight photos, which may be older than the loaded message window).
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
                )
            })
            .take(8)
            .filter(|m| {
                (m.media_path.is_some() || (live && m.media_url.starts_with("https://")))
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
    fn sync_composer_link(&mut self, text: &str) {
        let url = crate::media::first_link(text).filter(|u| {
            let host = crate::media::host_of(u);
            host.contains('.') && host.len() > 3 && !host.ends_with('.')
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

    /// Voice bubble ▶ / ■: download once to the media cache, play with
    /// `ffplay` (AMR goes through Zalo's `/mp3?url=` like Zalo PC).
    pub(crate) fn toggle_voice(&mut self, msg: &ChatMsg, cx: &mut Context<Self>) {
        if let Some((id, mut child)) = self.voice_playing.take() {
            let _ = child.kill();
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
                    std::process::Command::new("ffplay")
                        .args(["-nodisp", "-autoexit", "-loglevel", "quiet"])
                        .arg(&path)
                        .stdin(std::process::Stdio::null())
                        .spawn()
                        .map_err(|e| format!("ffplay: {e}"))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match ready {
                    Ok(child) => {
                        view.voice_playing = Some((id, child));
                        view.note = None;
                        // Clear the ■ state once ffplay exits.
                        cx.spawn(async move |this, cx| loop {
                            gpui::Timer::after(std::time::Duration::from_millis(400)).await;
                            let done = this
                                .update(cx, |view, cx| {
                                    let finished = match view.voice_playing.as_mut() {
                                        Some((pid, child)) if *pid == id => {
                                            !matches!(child.try_wait(), Ok(None))
                                        }
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
        let dir = self.session_dir.clone().or_else(crate::data::config_dir);
        if server_logout {
            if let Some(d) = dir.clone().filter(|_| self.has_session) {
                crate::bridge::spawn_live(crate::bridge::LiveAction::Logout, d);
            }
        }
        if let Some(d) = dir {
            crate::data::wipe_session_file(&d);
        }
        self.has_session = false;
        self.dialog = None;
        self.menu = None;
        crate::replace_shell_with_login(cx);
        let _ = window;
    }

    /// Start background batches for everything in [`Self::media_queue`].
    pub(crate) fn drain_media_queue(&mut self, cx: &mut Context<Self>) {
        if self.media_queue.is_empty() {
            return;
        }
        let dir = self.session_dir.clone();
        let batches = crate::fetch::plan(std::mem::take(&mut self.media_queue));
        cx.spawn(async move |this, cx| {
            for batch in batches {
                let dir = dir.clone();
                let done = cx
                    .background_executor()
                    .spawn(async move { crate::fetch::run_batch(dir.as_deref(), batch) })
                    .await;
                let alive = this.update(cx, |view, cx| {
                    view.apply_media(done);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn apply_media(&mut self, done: Vec<crate::fetch::Done>) {
        let max = f32::from(self.msg_scroll.max_offset().height);
        let at_bottom = -f32::from(self.msg_scroll.offset().y) >= max - 48.0;
        for item in done {
            match item.image {
                Some(image) => {
                    self.media_bitmaps.insert(item.msg_id, image);
                }
                None => {
                    self.media_failed.insert(item.msg_id);
                }
            }
            if let Some(path) = item.path {
                for msgs in self.messages.values_mut() {
                    if let Some(msg) = msgs.iter_mut().find(|m| m.id == item.msg_id) {
                        msg.media_path = Some(path.clone());
                        break;
                    }
                }
            }
            // Keep failed ids pending so a dead URL is not retried every open.
        }
        if at_bottom {
            self.msg_scroll.scroll_to_bottom();
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

    /// Queue a screen popup. `body` is clipped so the card stays one line.
    pub(crate) fn show_msg_notice(
        &mut self,
        title: String,
        body: String,
        initial: String,
        image: Option<Arc<RenderImage>>,
    ) {
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
        self.pending_notice = Some(crate::notice::NoticeContent {
            title,
            body,
            initial,
            image,
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
        crate::notice::present(cx, window.scale_factor(), content, &corner, x, y);
    }

    pub(crate) fn set_rail(&mut self, rail: Rail) {
        if rail == Rail::Settings {
            self.settings_open = true;
            self.refresh_settings_cache(self.settings_page);
            self.sticker_open = false;
            self.menu = None;
            return;
        }
        self.rail = rail;
        self.note = None;
        self.sticker_open = false;
        self.menu = None;
        self.forward_of = None;
        self.settings_open = false;
        if rail == Rail::Docs {
            if let Some(id) = self.cloud_id() {
                self.selected = Some(id);
                self.warm_media(id);
                self.msg_scroll.scroll_to_bottom();
            }
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
            RtEvent::Online => self.rt_status = None,
            RtEvent::Offline(why) => {
                eprintln!("zalo-gpui realtime offline: {why}");
                if crate::login_flow::is_session_error(&why) {
                    self.has_session = false;
                    self.rt_status =
                        Some("Phiên đăng nhập đã hết hạn — bấm «Đăng nhập lại» trên thanh thông báo để quét QR".into());
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
                self.rt_status = Some("Phiên này đã được mở ở nơi khác — ngừng nhận tin".into());
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
            RtEvent::SyncSignal(cmd, data) => self.on_sync_signal(cmd, data),
            RtEvent::BackupReply(cmd, data) => crate::backup_msg::on_backup_reply(cmd, data),
            RtEvent::Messages(list, live) => {
                let active = window.is_window_active();
                for m in list {
                    self.apply_incoming(m, live, active);
                }
                self.flush_notice(window, cx);
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
                let name = self
                    .profiles
                    .map
                    .get(&m.thread_id)
                    .map(|p| p.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| {
                        if m.is_group {
                            "Nhóm".to_owned()
                        } else {
                            m.sender_name.clone()
                        }
                    });
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
                self.avatars_queued = true;
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
            if viewing {
                self.send_seen_if_due(&m.thread_id);
            }
        }
        if let Some(gid) = m.global_msg_id {
            let id = self.messages.get_mut(&conv_id).and_then(|list| {
                let row = list.iter_mut().find(|x| x.global_msg_id == Some(gid))?;
                if m.kind != crate::media::MsgKind::Text && row.kind == crate::media::MsgKind::Text
                {
                    row.kind = m.kind;
                    row.text = m.text.clone();
                    if !m.file_name.is_empty() {
                        row.file_name = m.file_name.clone();
                    }
                    if !m.media_url.is_empty() {
                        row.media_url = m.media_url.clone();
                    }
                    if !m.href.is_empty() {
                        row.href = m.href.clone();
                    }
                    if m.width > 0 {
                        row.media_w = m.width;
                    }
                    if m.height > 0 {
                        row.media_h = m.height;
                    }
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
                Some(row.id)
            });
            if let Some(id) = id {
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
        msg.media_url = m.media_url.clone();
        msg.media_w = m.width;
        msg.media_h = m.height;
        msg.file_name = m.file_name.clone();
        msg.file_size = m.file_size;
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
            msg.href = m.text.clone();
            if !m.link_title.is_empty() || !m.media_url.is_empty() {
                self.link_previews.insert(
                    m.text.clone(),
                    crate::links::LinkPreview {
                        title: m.link_title.clone(),
                        desc: m.link_desc.clone(),
                        thumb: m.media_url.clone(),
                        src: crate::media::host_of(&m.text).to_owned(),
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
        let list = self.messages.entry(conv_id).or_default();
        let pos = list
            .iter()
            .position(|x| x.at_ms > m.at_ms && x.at_ms > 0)
            .unwrap_or(list.len());
        list.insert(pos, msg);
        let newest = pos + 1 == list.len();
        let viewing =
            self.selected == Some(conv_id) && active && !self.locked && self.rail == Rail::Chats;
        if let Some(i) = self.convs.iter().position(|c| c.id == conv_id) {
            let mut conv = self.convs.remove(i);
            if live && !m.from_me {
                if viewing {
                    conv.unread = 0;
                    self.marked_read.insert(conv_id);
                    self.mark_thread_read_pref(&m.thread_id, m.at_ms);
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
                let initial = conv
                    .name
                    .chars()
                    .next()
                    .map(|ch| ch.to_string())
                    .unwrap_or_else(|| "Z".into());
                let image = self.avatar_of(&conv.thread_id);
                self.show_msg_notice(conv.name.clone(), body, initial, image);
            }
        }
        if self.selected == Some(conv_id) {
            self.warm_media(conv_id);
            if live || newest {
                self.msg_scroll.scroll_to_bottom();
            }
        }
    }

    /// Pinned / muted / Other live in `ui-state.json` (same keys and thread
    /// ids as zalo-gui); hidden / removed in `gpui-prefs.json`.
    fn load_conv_flags(&mut self) {
        let ids = |key: &str| -> Vec<String> {
            serde_json::from_str::<Vec<String>>(&self.prefs.raw(key)).unwrap_or_default()
        };
        let map: std::collections::HashMap<String, u64> = self
            .convs
            .iter()
            .map(|c| (c.thread_id.clone(), c.id))
            .collect();
        let to_ids = |list: Vec<String>| -> HashSet<u64> {
            list.iter().filter_map(|t| map.get(t).copied()).collect()
        };
        self.pinned = to_ids(ids("pinned"));
        self.muted = to_ids(ids("muted"));
        self.archived = to_ids(ids("other"));
        self.hidden = to_ids(ids("hidden"));
        self.removed = to_ids(ids("removed"));
        self.starred = parse_starred_ids(&self.prefs.raw("starredMsgs"));
    }

    /// Persist the per-conversation flags (see [`Self::load_conv_flags`]).
    pub(crate) fn save_conv_flags(&mut self) {
        let threads = |set: &HashSet<u64>| -> serde_json::Value {
            let mut v: Vec<String> = self
                .convs
                .iter()
                .filter(|c| set.contains(&c.id) && data::live_allowed(&c.thread_id))
                .map(|c| c.thread_id.clone())
                .collect();
            v.sort();
            serde_json::json!(v)
        };
        let (p, m, o, h, r) = (
            threads(&self.pinned),
            threads(&self.muted),
            threads(&self.archived),
            threads(&self.hidden),
            threads(&self.removed),
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
        for r in list {
            let Some(conv_id) = self
                .convs
                .iter()
                .find(|c| c.thread_id == r.thread_id)
                .map(|c| c.id)
            else {
                continue;
            };
            let Some(msgs) = self.messages.get_mut(&conv_id) else {
                continue;
            };
            let Some(msg) = msgs.iter_mut().find(|m| {
                r.target_global_id
                    .is_some_and(|g| m.global_msg_id == Some(g))
                    || r.target_cli_id.is_some_and(|c| m.cli_msg_id == Some(c))
            }) else {
                continue;
            };
            crate::read_sync::update_reaction(
                &mut self.reactions,
                msg.id,
                &r.sender_uid,
                &r.r_icon,
                r.r_type,
                r.cleared,
            );
            msg.liked = self.reactions.get(&msg.id).is_some_and(|v| !v.is_empty());
        }
    }

    pub(crate) fn apply_server_pinned(&mut self, raw_ids: &[String]) {
        Self::sync_live_set(&self.convs, &mut self.pinned, raw_ids);
        self.save_conv_flags();
    }

    pub(crate) fn apply_server_archived(&mut self, raw_ids: &[String]) {
        Self::sync_live_set(&self.convs, &mut self.archived, raw_ids);
        self.save_conv_flags();
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
        let bounds = gpui::Bounds::centered(None, outer, cx);
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
            self.msg_scroll.scroll_to_bottom();
        }
    }

    /// Force the next render to refetch profiles (new group / friend).
    pub(crate) fn profiles_stale(&mut self) {
        self.profiles.saved_at = 0;
        self.profiles_refresh = self.has_session;
    }

    /// Clipboard image → temp PNG/JPEG → pending photo above the composer.
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
        let opened = crate::photo_window::open(
            cx,
            name,
            items,
            id,
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
                                if let Some(path) = crate::profiles::cache_path() {
                                    p.save(&path);
                                }
                                if view.display_name.is_none() {
                                    view.display_name = p.map.get(crate::profiles::ME).map(|m| m.name.clone()).filter(|n| !n.is_empty());
                                }
                                view.profiles = p;
                                view.apply_profile_aliases();
                                view.avatars_queued = true;
                            }
                            Err(err) => {
                                eprintln!("zalo-gpui profiles: {err}");
                                if crate::login_flow::is_session_error(&err) {
                                    view.has_session = false;
                                    view.set_note(
                                        "Phiên đăng nhập đã hết hạn — bấm «Đăng nhập lại» trên thanh thông báo để quét QR",
                                    );
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
        // Only the avatars the window can show: threads, recent senders, me.
        let mut keys: Vec<String> = self.convs.iter().map(|c| c.thread_id.clone()).collect();
        keys.push(crate::profiles::ME.into());
        if self.avatar_all {
            keys.extend(self.profiles.map.keys().cloned());
        }
        for msgs in self.messages.values() {
            for m in msgs.iter().rev().take(80) {
                if !m.sender_uid.is_empty() && !keys.contains(&m.sender_uid) {
                    keys.push(m.sender_uid.clone());
                }
            }
        }
        let jobs: Vec<(String, crate::fetch::Job)> = keys
            .into_iter()
            .filter(|k| !self.avatars.contains_key(k))
            .filter_map(|k| {
                let url = profile_avatar(&self.profiles, &k)?;
                url.starts_with("https://").then_some((k, url))
            })
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
                for d in done {
                    if let (Some(img), Some(key)) = (d.image, keys.get(d.msg_id as usize)) {
                        view.avatars.insert(key.clone(), img);
                    }
                }
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
        let mut want = Vec::new();
        for msgs in self.messages.values() {
            for msg in msgs {
                if !msg.sender_uid.is_empty() {
                    want.push(msg.sender_uid.clone());
                }
            }
        }
        for profile in self.profiles.map.values() {
            want.extend(profile.member_ids.iter().cloned());
        }
        for conv in &self.convs {
            if !conv.thread_id.is_empty() && !conv.thread_id.starts_with("g_") {
                want.push(conv.thread_id.clone());
            }
        }
        let missing: Vec<String> = crate::profiles::missing_avatar_uids(&self.profiles, &want)
            .into_iter()
            .take(80)
            .collect();
        self.profile_gaps_started = true;
        if missing.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move { crate::profiles::fetch_missing(&dir, &missing) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match got {
                    Ok(map) => {
                        let mut changed = false;
                        for (uid, got) in map {
                            let slot = view.profiles.map.entry(uid).or_default();
                            if got.avatar.starts_with("https://")
                                && !slot.avatar.starts_with("https://")
                            {
                                slot.avatar = got.avatar;
                                changed = true;
                            }
                            if slot.name.is_empty() && !got.name.is_empty() {
                                slot.name = got.name;
                                changed = true;
                            }
                        }
                        if changed {
                            if let Some(path) = crate::profiles::cache_path() {
                                view.profiles.save(&path);
                            }
                            view.avatars_queued = true;
                        }
                    }
                    Err(err) => eprintln!("zalo-gpui profile gaps: {err}"),
                }
                cx.notify();
            });
        })
        .detach();
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
        match std::process::Command::new("xdg-open").arg(&dir).spawn() {
            Ok(_) => self.set_note(
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
                row.name == "Cloud của tôi"
                    || self
                        .send2me_id
                        .as_deref()
                        .is_some_and(|id| id == row.thread_id)
            })
            .map(|row| row.id)
            .or_else(|| Some(preview::cloud_of_mine_id()))
    }

    /// Numeric Zalo user id (`uidMe`) of the logged-in account (`send2me_id`),
    /// distinct from `uin` which holds `getLoginInfo`'s 32-char `dkey`.
    pub(crate) fn self_uid(&self) -> Option<&str> {
        self.send2me_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
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

    pub(crate) fn listed_convs(&self) -> Vec<ChatConv> {
        let raw_query = self.search_query.trim();
        let query = raw_query.to_lowercase();
        let pin_match = self.pin_matches(raw_query);
        let mut rows: Vec<_> = self
            .convs
            .iter()
            .filter(|row| self.conv_visible_in_tab(row, &query, pin_match))
            .filter(|row| {
                conv_matches_list_filter(
                    self.list_filter,
                    &query,
                    self.is_stranger(row),
                    self.unread_of(row),
                    self.conv_label(row.id),
                )
            })
            .filter(|row| {
                query.is_empty()
                    || (pin_match && self.conv_hidden(row.id))
                    || row.name.to_lowercase().contains(&query)
                    || row.preview.to_lowercase().contains(&query)
            })
            .cloned()
            .collect();
        rows.sort_by_key(|row| {
            if self.pinned.contains(&row.id) {
                0u8
            } else {
                1
            }
        });
        rows
    }

    /// Esc: close the topmost overlay (menu → viewer → dialog → popup →
    /// settings → reply bar). Returns false when nothing was open.
    pub(crate) fn escape_overlay(&mut self) -> bool {
        if self.menu.take().is_some()
            || self.react_picker.take().is_some()
            || self.viewer.take().is_some()
            || self.dialog.take().is_some()
        {
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

    pub(crate) fn select_conv(&mut self, id: u64) {
        if self.selected != Some(id) {
            self.msg_window = MSG_WINDOW;
            self.select_msgs = None;
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
        }
        self.selected = Some(id);
        self.rail = Rail::Chats;
        self.note = None;
        self.sticker_open = false;
        self.settings_open = false;
        self.menu = None;
        self.react_picker = None;
        self.reply_to = None;
        self.forward_of = None;
        self.marked_read.insert(id);
        let last_at = self
            .messages
            .get(&id)
            .and_then(|l| l.last())
            .map(|m| m.at_ms)
            .unwrap_or(0)
            .max(data::now_ms());
        let mut live_thread = None;
        if let Some(conv) = self.convs.iter_mut().find(|c| c.id == id) {
            conv.unread = 0;
            if conv.live {
                live_thread = Some(conv.thread_id.clone());
            }
        }
        if let Some(thread) = live_thread {
            self.mark_thread_read_pref(&thread, last_at);
            self.send_seen_if_due(&thread);
        }
        self.warm_media(id);
        self.msg_scroll.scroll_to_bottom();
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

    pub(crate) fn send_draft(&mut self, text: &str, _cx: &mut Context<Self>) {
        let text = text.trim();
        if let Some(path) = self.pending_photo.take() {
            self.pending_thumb = None;
            self.send_photo_now(path, text);
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
                bridge::spawn_live(
                    LiveAction::Send {
                        thread_id: conv.thread_id.clone(),
                        is_group: conv.is_group,
                        text: text.to_owned(),
                        quote,
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
        if let Some(conv) = self.convs.iter_mut().find(|row| row.id == msg.conv) {
            conv.preview = msg.text.clone();
            conv.time = if msg.at_ms > 0 {
                data::vn_hm(msg.at_ms)
            } else {
                msg.time.clone()
            };
            conv.unread = 0;
        }
        self.messages.entry(msg.conv).or_default().push(msg);
        self.msg_scroll.scroll_to_bottom();
    }

    pub(crate) fn maybe_live(&self, action: LiveAction) {
        let allowed = match &action {
            LiveAction::Logout => true,
            LiveAction::Block { uid } => data::live_allowed(uid),
            LiveAction::Send { thread_id, .. }
            | LiveAction::SendContact { thread_id, .. }
            | LiveAction::Pin { thread_id, .. }
            | LiveAction::Mute { thread_id, .. }
            | LiveAction::Archive { thread_id, .. }
            | LiveAction::Undo { thread_id, .. }
            | LiveAction::Delete { thread_id, .. }
            | LiveAction::React { thread_id, .. }
            | LiveAction::Forward {
                dest_id: thread_id, ..
            }
            | LiveAction::SendPhoto { thread_id, .. }
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
        if self.capture_busy {
            return;
        }
        self.capture_busy = true;
        if hide_window {
            if let Some(wh) = window {
                let _ = wh.update(cx, |_, w, _| w.minimize_window());
            }
        }
        self.set_note("Chọn vùng cần chụp… (Esc để hủy)");
        cx.notify();
        cx.spawn(async move |this, cx| {
            // Let minimize animation (or context-menu close repaint) settle before grabbing.
            let wait_ms = if hide_window { 380 } else { 80 };
            gpui::Timer::after(std::time::Duration::from_millis(wait_ms)).await;

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

    /// Photo waiting above the composer; Enter sends it with the typed caption.
    pub(crate) fn set_pending_photo(&mut self, path: PathBuf) {
        self.pending_thumb = crate::media::decode_thumb(&path, 160);
        self.pending_photo = Some(path);
    }

    pub(crate) fn clear_pending_photo(&mut self) {
        self.pending_photo = None;
        self.pending_thumb = None;
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

    /// Local bubble + real upload (`upload_photo` → `send_photo`).
    pub(crate) fn send_photo_now(&mut self, path: PathBuf, caption: &str) {
        let Some(conv) = self.selected_conv() else {
            self.set_note("Chọn một hội thoại để gửi");
            return;
        };
        let now = data::now_ms();
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
        self.push_message(msg);
        self.warm_media(conv.id);
        if conv.live && self.has_session {
            self.maybe_live(LiveAction::SendPhoto {
                client_id: now as u64,
                thread_id: conv.thread_id,
                is_group: conv.is_group,
                path,
                caption: caption.to_owned(),
            });
            self.set_note("Đang gửi ảnh lên máy chủ…");
        } else {
            self.set_note("Hội thoại mẫu / chưa đăng nhập — ảnh chỉ hiện trên máy này");
        }
    }

    pub(crate) fn pick_and_attach(&mut self, photo: bool) {
        self.sticker_open = false;
        let picked = if photo {
            crate::plugins::pick_photo()
        } else {
            crate::plugins::pick_file()
        };
        match picked {
            Ok(Some(path)) if photo => {
                self.set_pending_photo(path);
                self.set_note("Nhập chú thích (nếu cần) rồi nhấn Enter để gửi ảnh");
            }
            Ok(Some(path)) if crate::video::is_video_path(&path) => self.send_video_now(path),
            Ok(Some(path)) => self.send_file_now(path),
            Ok(None) => {}
            Err(err) => self.set_note(err),
        }
    }
}

impl ShellView {
    /// Dock the info column when it fits. Otherwise it floats and a click
    /// outside closes it. Widening restores the saved preference.
    fn sync_info_layout(&mut self, window: &Window) {
        let csd = if crate::window_frame::CsdCorners::from_window(window).any_pad() {
            zalo_design::WINDOW_SHADOW * 2.0
        } else {
            0.0
        };
        let width = (f32::from(window.viewport_size().width) - csd).max(0.0);
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
        self.warm_links(cx);
        self.warm_polls(cx);
        self.publish_badge();
        let shares = crate::photo_window::take_shares();
        if !shares.is_empty() {
            window.activate_window();
            for msg in shares {
                self.open_share(msg, cx);
            }
        }
        if info {
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
        self.drain_media_queue(cx);
        self.drain_sticker_thumbs(cx);
        self.drain_labels(cx);
        self.drain_profiles(cx);
        self.fill_profile_gaps(cx);
        self.drain_presence(cx);
        self.drain_media_store(cx);
        self.start_realtime(window, cx);
        self.start_shortcuts(window, cx);
        self.ensure_privacy_loaded(cx);
        self.sync_open_seen(window.is_window_active());
        let hint = match self.selected_conv() {
            Some(row) if self.english => format!("Type @, message to {}", row.name),
            Some(row) => format!("Nhập @, tin nhắn tới {}", row.name),
            None => str_vi("STR_SEND_MESSAGE", "Nhập tin nhắn").to_owned(),
        };
        self.composer
            .update(cx, |input, _| input.set_placeholder(hint));
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
        window.set_window_title(&main_title);
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
                                .child(self.sidebar_column(cx))
                                .child(self.chat_column(corners, cx))
                                .when(self.info_overlay && !self.msg_search_open, |el| {
                                    el.child(self.info_dismiss(cx))
                                })
                                .when(self.msg_search_open && selected.is_some(), |el| {
                                    el.child(self.msg_search_panel(selected.as_ref(), corners, cx))
                                })
                                .when(info && !self.msg_search_open, |el| {
                                    el.child(self.info_slot(selected.as_ref(), corners, cx))
                                }),
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
                .when(forward, |el| el.child(self.forward_overlay(cx)))
                .when_some(toast, |el, text| el.child(toast_el(text)))
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
                    .text_color(rgb(0xffffff))
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
    let tid = thread_id.trim();
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
    let next_id = convs.iter().map(|c| c.id).max().unwrap_or(0) + 1;
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

/// Shared `ListFilter` predicate used by `ShellView::listed_convs` and unit tests.
pub(crate) fn conv_matches_list_filter(
    filter: ListFilter,
    query: &str,
    stranger: bool,
    unread: u32,
    label: Option<i64>,
) -> bool {
    match filter {
        ListFilter::All => !query.is_empty() || !stranger,
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
    if send2me_id.is_some_and(|id| id == conv.thread_id)
        || uin.is_some_and(|id| id == conv.thread_id)
        || matches!(conv.name.as_str(), "Cloud của tôi" | "Zalo")
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
        assert_eq!(id, 1);
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
            Some(1)
        );
        assert_eq!(convs.len(), 1);

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

        // Folding: strangers stay out of default All view unless searching, and appear in Strangers view.
        assert!(!conv_matches_list_filter(
            ListFilter::All,
            "",
            true,
            2,
            None
        ));
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
}
