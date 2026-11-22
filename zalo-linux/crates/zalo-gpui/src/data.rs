//! Owned conversation / message rows for the GPUI shell.
//!
//! Preview fixtures stay in [`crate::preview`]. Runtime loads `store.db` and
//! `session.json` from XDG `zalo-rust` with no HTTP. A saved login with no
//! local store opens an empty shell and lets realtime fill it. Tests, and a
//! launch with no session, keep the preview fixtures.

use std::collections::HashMap;
#[cfg(not(test))]
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::media::{MsgKind, PollChoice};

use zalo_client::StoredSession;
#[cfg(not(test))]
use zalo_storage::ThreadKind;
use zalo_storage::{LocalStore, MessageRecord};

use crate::color::str_vi;
use crate::preview::{self, PreviewConv, PreviewMsg};

/// Vietnam UTC+7 offset used by `zalo-gui` for message timestamps.
const VN_OFFSET_MS: i64 = 7 * 3_600_000;

/// One conversation row in the 344 px list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatConv {
    /// Stable element id (preview keeps 1–4).
    pub id: u64,
    /// Storage / protocol thread id. Preview uses `preview:{id}`.
    pub thread_id: String,
    /// Display name.
    pub name: String,
    /// Last-message preview line.
    pub preview: String,
    /// Time column already formatted.
    pub time: String,
    /// Unread badge. Zero hides it.
    pub unread: u32,
    /// Group thread (header skips Phone).
    pub is_group: bool,
    /// Online dot on the avatar.
    pub online: bool,
    /// True when a user click may call `zalo-client` (real uid, not preview).
    pub live: bool,
}

/// One chat bubble.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatMsg {
    /// Local row id (`message_id` from SQLite, or a preview counter).
    pub id: u64,
    /// Parent conversation element id.
    pub conv: u64,
    /// Parent thread id (same as [`ChatConv::thread_id`]).
    pub thread_id: String,
    /// Right-side `--blue-message` when true.
    pub from_me: bool,
    /// Group sender label.
    pub sender: String,
    /// Protocol uid of the sender, empty on preview / own bubbles.
    pub sender_uid: String,
    /// Bubble body.
    pub text: String,
    /// `HH:mm` under the card.
    pub time: String,
    /// Unix milliseconds. Zero on hardcoded preview bubbles.
    pub at_ms: i64,
    /// Client message id, when the store has one.
    pub cli_msg_id: Option<i64>,
    /// Server message id, when the store has one.
    pub global_msg_id: Option<i64>,
    /// Local recall flag.
    pub recalled: bool,
    /// Local like flag.
    pub liked: bool,
    /// Parsed body kind (photo/file/link/…).
    pub kind: MsgKind,
    /// Cached image path, when the photo/sticker file is on disk.
    pub media_path: Option<PathBuf>,
    /// Declared photo width.
    pub media_w: u32,
    /// Declared photo height.
    pub media_h: u32,
    /// File name or link host.
    pub file_name: String,
    /// File size in bytes.
    pub file_size: u64,
    /// Link href.
    pub href: String,
    /// Remote thumb/HD URL for photo, sticker and GIF bubbles.
    pub media_url: String,
    /// Quoted message sender name (when this bubble replies to another).
    pub quote_sender: String,
    /// Quoted message text/placeholder snippet.
    pub quote_text: String,
    /// Quoted server `msgId` (`globalMsgId`).
    pub quote_global_id: Option<i64>,
    /// Quoted client `cliMsgId`.
    pub quote_cli_id: Option<i64>,
    /// `group.poll` id. `0` when this bubble is not a poll.
    pub poll_id: i64,
    /// `create` / `share` / `vote` / `update` / `close`.
    pub poll_action: String,
    /// Poll is closed (`status != 0`).
    pub poll_closed: bool,
    /// Choices. Empty until `/api/poll/detail` fills them.
    pub poll_options: Vec<PollChoice>,
    /// Electron `params.group_layout_id` for one cell of a multi-photo send.
    /// `0` means the row has no album id.
    pub group_layout_id: i64,
}

impl ChatMsg {
    /// A local text bubble (composer / preview / tests).
    #[allow(clippy::too_many_arguments)]
    pub fn text_msg(
        id: u64,
        conv: u64,
        thread_id: String,
        from_me: bool,
        sender: String,
        sender_uid: String,
        text: String,
        time: String,
        at_ms: i64,
        cli_msg_id: Option<i64>,
        global_msg_id: Option<i64>,
    ) -> Self {
        Self {
            id,
            conv,
            thread_id,
            from_me,
            sender,
            sender_uid,
            text,
            time,
            at_ms,
            cli_msg_id,
            global_msg_id,
            recalled: false,
            liked: false,
            kind: MsgKind::Text,
            media_path: None,
            media_w: 0,
            media_h: 0,
            file_name: String::new(),
            file_size: 0,
            href: String::new(),
            media_url: String::new(),
            quote_sender: String::new(),
            quote_text: String::new(),
            quote_global_id: None,
            quote_cli_id: None,
            poll_id: 0,
            poll_action: String::new(),
            poll_closed: false,
            poll_options: Vec::new(),
            group_layout_id: 0,
        }
    }
}

/// Exclusive end of a horizontal photo album that starts at `start`.
/// A single photo returns `start + 1`.
///
/// Electron groups cells that share `group_layout_id` (`is_group_layout` and
/// `total_item_in_group > 1`). Rows saved before that id was kept still belong
/// to one send when they are consecutive photos from the same person and each
/// neighbor is at most 2 seconds later — the gap of one composer send.
#[must_use]
pub fn photo_album_end(msgs: &[ChatMsg], start: usize) -> usize {
    const GAP_MS: i64 = 2_000;
    let Some(first) = msgs.get(start) else {
        return start;
    };
    if !is_album_photo(first) {
        return start + 1;
    }
    let layout = first.group_layout_id;
    let mut end = start + 1;
    while let Some(next) = msgs.get(end) {
        if !is_album_photo(next) || !same_album_sender(first, next) {
            break;
        }
        if layout != 0 {
            if next.group_layout_id != layout {
                break;
            }
        } else if next.group_layout_id != 0
            || next.at_ms == 0
            || msgs[end - 1].at_ms == 0
            || (next.at_ms - msgs[end - 1].at_ms).abs() > GAP_MS
        {
            break;
        }
        end += 1;
    }
    if end - start < 2 {
        start + 1
    } else {
        end
    }
}

fn is_album_photo(msg: &ChatMsg) -> bool {
    msg.kind == MsgKind::Photo && !msg.recalled
}

fn same_album_sender(a: &ChatMsg, b: &ChatMsg) -> bool {
    a.from_me == b.from_me && a.sender_uid == b.sender_uid
}

/// One Contacts person or group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatPerson {
    /// Display name.
    pub name: String,
    /// Online dot.
    pub online: bool,
    /// Group row.
    pub is_group: bool,
    /// Friend uid or group thread id. Empty on preview people.
    pub uid: String,
}

/// Local snapshot painted by [`crate::shell::ShellView`].
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Conversation list, newest first as stored.
    pub convs: Vec<ChatConv>,
    /// Bubbles keyed by conversation element id.
    pub messages: HashMap<u64, Vec<ChatMsg>>,
    /// Contacts friends.
    pub friends: Vec<ChatPerson>,
    /// Contacts groups.
    pub groups: Vec<ChatPerson>,
    /// Session display name, when `session.json` loaded.
    pub display_name: Option<String>,
    /// Account uin, when present.
    pub uin: Option<String>,
    /// "My Cloud" conversation id.
    pub send2me_id: Option<String>,
    /// XDG `zalo-rust` directory when it exists.
    pub session_dir: Option<PathBuf>,
    /// True when `zpw_sek` is present (user-click live actions allowed).
    pub has_session: bool,
    /// Next local message id.
    pub next_msg_id: u64,
}

/// FNV-1a of `thread_id`, skipping 0 and preview ids 1–4.
#[must_use]
pub fn hash_id(thread_id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in thread_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    if hash == 0 || (1..=4).contains(&hash) {
        hash = hash.wrapping_add(16);
    }
    hash
}

/// True when `thread_id` may be sent to `/api/*`.
///
/// Preview fixtures (`preview:1` … `preview:4` and bare `1`–`4`) stay local.
#[must_use]
pub fn live_allowed(thread_id: &str) -> bool {
    let id = thread_id.trim();
    if id.is_empty() || id.starts_with("preview:") {
        return false;
    }
    !matches!(id, "1" | "2" | "3" | "4")
}

/// Machine config directory: `~/.config/zalo-rust` on Linux,
/// `~/Library/Application Support/zalo-rust` on macOS.
///
/// Login writes `session.json` here via [`zalo_core::AppPaths`]. The shell must
/// read the same directory or a successful QR login opens the preview fixtures.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    zalo_core::AppPaths::from_env("zalo-rust")
        .ok()
        .map(|paths| paths.config_dir().to_path_buf())
}

/// Directory of one account: `accounts/<userId>` under the machine config dir.
///
/// The id is the `userId` from `profile/me`. Characters that are not safe in a
/// path are replaced so the folder cannot escape the accounts directory.
#[must_use]
pub fn account_dir(machine: &std::path::Path, user_id: &str) -> PathBuf {
    let safe: String = user_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    machine.join("accounts").join(safe)
}

/// `current-user` names the account directory the UI is allowed to open.
#[must_use]
pub fn read_current_user(machine: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(machine.join("current-user"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Points the UI at `user_id`. Does not delete any other account directory.
pub fn write_current_user(machine: &std::path::Path, user_id: &str) {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(machine);
    let _ = std::fs::write(machine.join("current-user"), user_id);
}

/// Drops the pointer. A later login of a different account cannot see this one.
pub fn clear_current_user(machine: &std::path::Path) {
    let _ = std::fs::remove_file(machine.join("current-user"));
}

/// Session directory to open.
///
/// A `current-user` pointer wins. If it names an account whose `session.json`
/// is gone, this returns `None` instead of falling back to a leftover session
/// at the machine root. The root session is only for installs that have never
/// written `current-user`.
#[must_use]
pub fn active_data_dir() -> Option<PathBuf> {
    let machine = config_dir()?;
    if let Some(uid) = read_current_user(&machine) {
        let dir = account_dir(&machine, &uid);
        return dir.join("session.json").is_file().then_some(dir);
    }
    machine.join("session.json").is_file().then_some(machine)
}

/// Copies `device.json` into an account directory.
///
/// API calls look for the device file beside `session.json`. Generating a
/// second file would mint a new IMEI and the server would treat it as another
/// computer.
pub fn share_device(machine: &std::path::Path, account: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(account)?;
    let src = machine.join("device.json");
    let dest = account.join("device.json");
    if dest.is_file() || !src.is_file() {
        return Ok(());
    }
    std::fs::copy(src, dest).map(|_| ())
}

/// Moves this machine's legacy store into `account` when `account.uid` is this user.
///
/// A mismatched id is left where it is. Copying it would put the previous
/// account's chats into the new account.
pub fn migrate_legacy_store(machine: &std::path::Path, account: &std::path::Path, user_id: &str) {
    if account.join("store.db").is_file() {
        return;
    }
    if read_last_account_uid(machine).as_deref() != Some(user_id) {
        return;
    }
    for name in ["store.db", "store.db-wal", "store.db-shm", "queue-cursors.json"] {
        let src = machine.join(name);
        if src.is_file() {
            let _ = std::fs::rename(&src, account.join(name));
        }
    }
}

/// True when `session.json` holds a `zpw_sek`. Tests always return false.
#[must_use]
pub fn session_present() -> bool {
    if cfg!(test) {
        return false;
    }
    active_data_dir()
        .and_then(|dir| StoredSession::load(&dir).ok())
        .and_then(|session| session.zpw_sek())
        .is_some()
}

/// Deletes `session.json` in `dir`. `device.json` stays so the next QR keeps the IMEI.
pub fn wipe_session_file(dir: &std::path::Path) {
    let _ = std::fs::remove_file(dir.join("session.json"));
}

/// Logout: drop the active pointer and this account's bearer session.
///
/// The account directory's `store.db` stays, so the same user can log in again.
/// Other accounts are not touched. The legacy root `session.json` is removed
/// too, otherwise a cleared pointer would reopen the previous account.
pub fn detach_account(session_dir: &std::path::Path) {
    wipe_session_file(session_dir);
    // `accounts/<userId>` → the machine config dir is two levels up.
    // A legacy session lives in the machine dir itself.
    let machine = if session_dir
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|name| name == "accounts")
    {
        session_dir.parent().and_then(|p| p.parent())
    } else {
        Some(session_dir)
    };
    if let Some(machine) = machine {
        clear_current_user(machine);
        if machine != session_dir {
            wipe_session_file(machine);
        }
    }
}

/// Deletes ALL account-scoped local state in `dir` (and matching profile/link caches)
/// while keeping `device.json` and app-level UI preferences (theme, language, window size).
///
/// Called on explicit user logout and whenever QR login authenticates a different account
/// than the previous one, so chats/groups/cursors/profiles from the old account never leak.
#[cfg(test)]
pub fn wipe_account_data(dir: &std::path::Path) {
    wipe_session_file(dir);
    for name in [
        "store.db",
        "store.db-wal",
        "store.db-shm",
        "queue-cursors.json",
        "account.uid",
    ] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let mut prefs = crate::prefs::Prefs::load(Some(dir.to_path_buf()));
    prefs.reset_account_keys();
    if config_dir().as_deref() == Some(dir) {
        if let Some(p) = crate::profiles::cache_path() {
            let _ = std::fs::remove_file(p);
        }
        if let Some(l) = crate::links::cache_path() {
            let _ = std::fs::remove_file(l);
        }
    }
}

/// Reads the last logged-in account identifier stored in `dir/account.uid`.
#[must_use]
pub fn read_last_account_uid(dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(dir.join("account.uid"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Persists the active account identifier to `dir/account.uid`.
pub fn write_last_account_uid(dir: &std::path::Path, uid: &str) {
    let uid = uid.trim();
    if !uid.is_empty() {
        let _ = std::fs::write(dir.join("account.uid"), uid);
    }
}

/// Unix milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `HH:mm` in Vietnam time.
#[must_use]
pub fn vn_hm(ms: i64) -> String {
    let tod = (ms + VN_OFFSET_MS).rem_euclid(86_400_000);
    format!("{:02}:{:02}", tod / 3_600_000, (tod % 3_600_000) / 60_000)
}

/// Conversation-list time column: today `HH:mm`, yesterday "Yesterday", else `dd/mm`.
#[must_use]
#[allow(dead_code)]
pub fn conv_time(at_ms: i64) -> String {
    if at_ms <= 0 {
        return String::new();
    }
    match vn_day(now_ms()) - vn_day(at_ms) {
        0 => vn_hm(at_ms),
        1 => str_vi("STR_YESTERDAY_RAW", "Hôm qua").to_owned(),
        _ => {
            let (_, month, day) = vn_ymd(at_ms);
            format!("{day:02}/{month:02}")
        }
    }
}

/// Conversation-list time, Zalo PC `toTimestampString` rules: "Just now",
/// "N minutes", "N hours", "Yesterday" / "N days" (≤ 7 days), `dd/MM`, then
/// «dd/MM/yy» after a year.
#[must_use]
pub fn conv_time_rel(at_ms: i64, now_ms: i64) -> String {
    if at_ms <= 0 {
        return String::new();
    }
    let diff = (now_ms - at_ms).max(0) / 1000;
    let (year, month, day) = vn_ymd(at_ms);
    let (now_year, _, _) = vn_ymd(now_ms);
    if diff < 60 {
        return str_vi("STR_FEW_SECONDS", "Vài giây").to_owned();
    }
    if diff < 3600 {
        return if crate::i18n::is_english() {
            format!("{} min", diff / 60)
        } else {
            format!("{} phút", diff / 60)
        };
    }
    if diff < 86_400 {
        return if crate::i18n::is_english() {
            format!("{} h", diff / 3600)
        } else {
            format!("{} giờ", diff / 3600)
        };
    }
    let days = diff / 86_400;
    if days >= 365 || year != now_year && days > 7 {
        return format!("{day:02}/{month:02}/{:02}", year % 100);
    }
    if days > 7 {
        return format!("{day:02}/{month:02}");
    }
    if vn_day(now_ms) - vn_day(at_ms) == 1 {
        return str_vi("STR_YESTERDAY_RAW", "Hôm qua").to_owned();
    }
    if crate::i18n::is_english() {
        format!("{days} d")
    } else {
        format!("{days} ngày")
    }
}

/// `.chat-date` chip: `HH:mm Today` / `Yesterday` / `dd/mm/yyyy`.
#[must_use]
pub fn day_label(at_ms: i64, now_ms: i64) -> String {
    let gap = vn_day(now_ms) - vn_day(at_ms);
    let day = match gap {
        0 => str_vi("STR_TODAY", "Hôm nay").to_owned(),
        1 => str_vi("STR_YESTERDAY_RAW", "Hôm qua").to_owned(),
        _ => {
            let (year, month, day) = vn_ymd(at_ms);
            format!("{day:02}/{month:02}/{year}")
        }
    };
    format!("{} {day}", vn_hm(at_ms))
}

pub(crate) fn vn_day(ms: i64) -> i64 {
    (ms + VN_OFFSET_MS).div_euclid(86_400_000)
}

pub(crate) fn vn_ymd(ms: i64) -> (i64, i64, i64) {
    let days = vn_day(ms);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as i64, day as i64)
}

#[cfg(not(test))]
fn session_self_uids(session: Option<&StoredSession>) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Some(s) = session {
        for opt in [&s.uin, &s.user_id, &s.send2me_id] {
            if let Some(id) = opt.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
                set.insert(id.to_owned());
            }
        }
    }
    set
}

/// Conversations and messages currently in `dir/store.db`.
#[must_use]
pub(crate) fn snapshot_from_store(dir: &std::path::Path) -> Option<Snapshot> {
    #[cfg(test)]
    {
        let _ = dir;
        None
    }
    #[cfg(not(test))]
    {
        let session = StoredSession::load(dir).ok();
        let self_uids = session_self_uids(session.as_ref());
        let store = LocalStore::open(&dir.join("store.db")).ok()?;
        let mut snap = map_store(&store, &self_uids);
        if let Some(s) = session {
            snap.display_name = s.display_name;
            snap.uin = s.uin;
            snap.send2me_id = s.send2me_id;
        }
        snap.session_dir = Some(dir.to_path_buf());
        snap.has_session = true;
        Some(snap)
    }
}

/// Load a full account snapshot from a specific session directory `dir`
/// (returning an empty logged-in shell when `store.db` does not exist yet).
#[must_use]
pub(crate) fn load_snapshot_from_dir(dir: &std::path::Path) -> Option<Snapshot> {
    #[cfg(test)]
    {
        let _ = dir;
        None
    }
    #[cfg(not(test))]
    {
        load_from_dir(dir).or_else(|| {
            let session = StoredSession::load(dir).ok()?;
            session
                .zpw_sek()
                .is_some()
                .then(|| snapshot_from_session(dir, &session))
        })
    }
}

/// Load local content. Tests always get the four preview threads.
#[must_use]
pub fn load_snapshot() -> Snapshot {
    #[cfg(test)]
    {
        return preview_snapshot();
    }
    #[cfg(not(test))]
    {
        load_from_disk()
            .unwrap_or_else(|| session_shell_snapshot().unwrap_or_else(preview_snapshot))
    }
}

/// Logged-in shell with no local threads yet. Realtime `startup_sync` fills it.
#[cfg(not(test))]
fn session_shell_snapshot() -> Option<Snapshot> {
    let dir = active_data_dir()?;
    let session = StoredSession::load(&dir).ok()?;
    session
        .zpw_sek()
        .is_some()
        .then(|| snapshot_from_session(&dir, &session))
}

/// Empty conversation list bound to a real session directory.
fn snapshot_from_session(dir: &std::path::Path, session: &StoredSession) -> Snapshot {
    let mut snap = Snapshot {
        convs: Vec::new(),
        messages: HashMap::new(),
        friends: Vec::new(),
        groups: Vec::new(),
        display_name: session.display_name.clone(),
        uin: session.uin.clone(),
        send2me_id: session.send2me_id.clone(),
        session_dir: Some(dir.to_path_buf()),
        has_session: session.zpw_sek().is_some(),
        next_msg_id: 1,
    };
    attach_saved_friends(&mut snap, session.uin.as_deref());
    snap
}

#[cfg(not(test))]
fn attach_saved_friends(snap: &mut Snapshot, uin: Option<&str>) {
    if let Some(uin) = uin {
        merge_index_friends(snap, uin);
    }
}

#[cfg(test)]
fn attach_saved_friends(_snap: &mut Snapshot, _uin: Option<&str>) {}

/// Hardcoded chrome used when `store.db` is missing and in unit tests.
#[must_use]
pub fn preview_snapshot() -> Snapshot {
    let convs: Vec<ChatConv> = preview::PREVIEW.iter().map(conv_from_preview).collect();
    let mut messages: HashMap<u64, Vec<ChatMsg>> = HashMap::new();
    for msg in preview::PREVIEW_MSGS {
        messages
            .entry(msg.conv)
            .or_default()
            .push(msg_from_preview(msg));
    }
    let friends: Vec<ChatPerson> = preview::FRIENDS.iter().map(person_from_preview).collect();
    let groups: Vec<ChatPerson> = preview::GROUPS.iter().map(person_from_preview).collect();
    Snapshot {
        convs,
        messages,
        friends,
        groups,
        display_name: None,
        uin: None,
        send2me_id: None,
        session_dir: None,
        has_session: false,
        next_msg_id: 1_000,
    }
}

fn conv_from_preview(row: &PreviewConv) -> ChatConv {
    ChatConv {
        id: row.id,
        thread_id: format!("preview:{}", row.id),
        name: row.name.to_owned(),
        preview: row.preview.to_owned(),
        time: row.time.to_owned(),
        unread: row.unread,
        is_group: row.is_group,
        online: row.online,
        live: false,
    }
}

fn msg_from_preview(msg: &PreviewMsg) -> ChatMsg {
    ChatMsg::text_msg(
        msg.conv * 10 + u64::from(msg.from_me) + msg.text.len() as u64,
        msg.conv,
        format!("preview:{}", msg.conv),
        msg.from_me,
        msg.sender.to_owned(),
        String::new(),
        msg.text.to_owned(),
        msg.time.to_owned(),
        0,
        None,
        None,
    )
}

fn person_from_preview(row: &preview::PreviewPerson) -> ChatPerson {
    ChatPerson {
        name: row.name.to_owned(),
        online: row.online,
        is_group: row.is_group,
        uid: String::new(),
    }
}

#[cfg(not(test))]
fn load_from_disk() -> Option<Snapshot> {
    let dir = active_data_dir()?;
    load_from_dir(&dir)
}

#[cfg(not(test))]
fn load_from_dir(dir: &std::path::Path) -> Option<Snapshot> {
    let session = StoredSession::load(dir).ok()?;
    if session.zpw_sek().is_none() {
        return None;
    }
    let store_path = dir.join("store.db");
    if !store_path.is_file() {
        return None;
    }
    let self_uids = session_self_uids(Some(&session));
    let store = LocalStore::open(&store_path).ok()?;
    let mut snap = map_store(&store, &self_uids);
    if snap.convs.is_empty() {
        return None;
    }
    snap.display_name = session.display_name.clone();
    snap.uin = session.uin.clone();
    snap.send2me_id = session.send2me_id.clone();
    snap.has_session = true;
    snap.session_dir = Some(dir.to_path_buf());
    if let Some(sid) = snap.send2me_id.as_deref().filter(|s| !s.is_empty()) {
        if let Some(existing) = snap.convs.iter_mut().find(|c| c.thread_id == sid) {
            existing.name = "My Documents".to_owned();
        } else {
            snap.convs.push(ChatConv {
                id: hash_id(sid),
                thread_id: sid.to_owned(),
                name: "My Documents".to_owned(),
                preview: String::new(),
                time: String::new(),
                unread: 0,
                is_group: false,
                online: false,
                live: true,
            });
        }
    }
    for conv in &mut snap.convs {
        if conv.name == "Cloud của tôi" || conv.name == "My Cloud" {
            conv.name = "My Documents".to_owned();
        }
    }
    if let Some(uin) = session.uin.as_deref() {
        merge_index_friends(&mut snap, uin);
    }
    Some(snap)
}

#[cfg(not(test))]
fn merge_index_friends(snap: &mut Snapshot, uin: &str) {
    let Some(db) = zalo_storage::zalo_pc_index_db_default(uin) else {
        return;
    };
    let Ok(rows) = zalo_storage::read_zalo_pc_friends(&db, uin) else {
        return;
    };
    for row in rows {
        let uid = row.text.get("userId").cloned().unwrap_or_default();
        if uid.is_empty() {
            continue;
        }
        if snap.friends.iter().any(|p| p.uid == uid) {
            continue;
        }
        let name = row
            .text
            .get("displayName")
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uid.clone());
        let online = row.number.get("isActivePC").copied().unwrap_or(0) != 0
            || row.number.get("isActive").copied().unwrap_or(0) != 0;
        snap.friends.push(ChatPerson {
            name,
            online,
            is_group: false,
            uid,
        });
    }
    snap.friends.sort_by(|a, b| a.name.cmp(&b.name));
}

#[cfg(not(test))]
#[allow(clippy::too_many_arguments)]
fn record_to_chat_msg(
    rec: &MessageRecord,
    mid: u64,
    conv_id: u64,
    thread_id: &str,
    users: &HashMap<String, String>,
    attachments: &HashMap<i64, String>,
    self_uids: &HashSet<String>,
    you: &str,
) -> ChatMsg {
    let raw_sender = rec.sender_id.as_deref().map(str::trim).unwrap_or("");
    let from_me = raw_sender.is_empty() || raw_sender == "0" || self_uids.contains(raw_sender);
    let sender_uid = if from_me {
        String::new()
    } else {
        raw_sender.to_owned()
    };
    let sender = if from_me {
        you.to_owned()
    } else {
        let name = users.get(&sender_uid).map(String::as_str).unwrap_or("");
        crate::media::sender_label(&sender_uid, name)
    };
    let attach = attachments.get(&rec.message_id).map(String::as_str);
    let meta = crate::media::classify(&rec.body, attach);
    ChatMsg {
        id: mid,
        conv: conv_id,
        thread_id: thread_id.to_owned(),
        from_me,
        sender,
        sender_uid,
        text: rec.body.clone(),
        time: vn_hm(rec.sent_at),
        at_ms: rec.sent_at,
        cli_msg_id: rec.cli_msg_id,
        global_msg_id: rec.global_msg_id,
        recalled: false,
        liked: false,
        kind: meta.kind,
        media_path: meta.path,
        media_w: meta.width,
        media_h: meta.height,
        file_name: meta.title,
        file_size: meta.size,
        href: meta.href,
        media_url: meta.url,
        quote_sender: meta.quote_sender,
        quote_text: meta.quote_text,
        quote_global_id: meta.quote_global_id,
        quote_cli_id: meta.quote_cli_id,
        poll_id: meta.poll_id,
        poll_action: meta.poll_action,
        poll_closed: meta.poll_closed,
        poll_options: meta.poll_options,
        group_layout_id: meta.group_layout_id,
    }
}

/// Full-text + date-range search inside `thread_id`'s persisted `store.db`
/// message history (`searchForEmpty: true` matching Zalo Electron).
#[must_use]
pub fn search_thread_messages_filtered(
    thread_id: &str,
    query: &str,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    limit: usize,
) -> Vec<ChatMsg> {
    #[cfg(test)]
    {
        let _ = (thread_id, query, from_ms, to_ms, limit);
        Vec::new()
    }
    #[cfg(not(test))]
    {
        let q = query.trim();
        if q.is_empty() && from_ms.is_none() && to_ms.is_none() {
            return Vec::new();
        }
        let Some(dir) = active_data_dir() else {
            return Vec::new();
        };
        let session = StoredSession::load(&dir).ok();
        let self_uids = session_self_uids(session.as_ref());
        let store_path = dir.join("store.db");
        let Ok(store) = LocalStore::open(&store_path) else {
            return Vec::new();
        };
        let Ok(recs) = store.search_thread_filtered(thread_id, q, from_ms, to_ms, limit) else {
            return Vec::new();
        };
        let users: HashMap<String, String> = store
            .users()
            .unwrap_or_default()
            .into_iter()
            .map(|u| (u.user_id, u.display_name))
            .collect();
        let attachments: HashMap<i64, String> = store
            .attachments()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let you = str_vi("STR_YOU", "Bạn");
        let conv_id = hash_id(thread_id);
        let mut out: Vec<ChatMsg> = recs
            .into_iter()
            .filter(|rec| {
                rec.body.trim() != "sendBubbleMessage"
                    || attachments.contains_key(&rec.message_id)
            })
            .enumerate()
            .map(|(i, rec)| {
                let mid = u64::try_from(rec.message_id).unwrap_or(1_000_000 + i as u64);
                record_to_chat_msg(
                    &rec,
                    mid,
                    conv_id,
                    thread_id,
                    &users,
                    &attachments,
                    &self_uids,
                    you,
                )
            })
            .collect();
        out.sort_by_key(|m| m.at_ms);
        out
    }
}

#[cfg(not(test))]
fn map_store(store: &LocalStore, self_uids: &HashSet<String>) -> Snapshot {
    let users: HashMap<String, String> = store
        .users()
        .unwrap_or_default()
        .into_iter()
        .map(|user| (user.user_id, user.display_name))
        .collect();
    let attachments: HashMap<i64, String> = store
        .attachments()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let you = str_vi("STR_YOU", "Bạn");
    let mut convs = Vec::new();
    let mut messages = HashMap::new();
    let mut next_msg_id = 1_000_u64;
    for thread in store.threads().unwrap_or_default() {
        let id = hash_id(&thread.thread_id);
        let live = live_allowed(&thread.thread_id);
        let mut msgs = Vec::new();
        for rec in store.messages(&thread.thread_id).unwrap_or_default() {
            if rec.body.trim() == "sendBubbleMessage" && !attachments.contains_key(&rec.message_id)
            {
                continue;
            }
            let mid = u64::try_from(rec.message_id).unwrap_or(next_msg_id);
            next_msg_id = next_msg_id.max(mid.saturating_add(1));
            msgs.push(record_to_chat_msg(
                &rec,
                mid,
                id,
                &thread.thread_id,
                &users,
                &attachments,
                self_uids,
                you,
            ));
        }
        let last = msgs.last();
        let preview_line = last
            .map(|m| {
                if crate::media::is_group_call_body(&m.text) {
                    "[Cuộc gọi nhóm]".to_owned()
                } else {
                    m.text.clone()
                }
            })
            .unwrap_or_default();
        let time = last
            .map(|m| {
                if m.at_ms > 0 {
                    conv_time(m.at_ms)
                } else {
                    m.time.clone()
                }
            })
            .unwrap_or_default();
        convs.push(ChatConv {
            id,
            thread_id: thread.thread_id.clone(),
            name: thread.title,
            preview: preview_line,
            time,
            unread: u32::try_from(thread.unread.max(0)).unwrap_or(0),
            is_group: matches!(thread.kind, ThreadKind::Group),
            online: false,
            live,
        });
        messages.insert(id, msgs);
    }
    let friends: Vec<ChatPerson> = users
        .iter()
        .map(|(uid, name)| ChatPerson {
            name: name.clone(),
            online: false,
            is_group: false,
            uid: uid.clone(),
        })
        .collect();
    let groups: Vec<ChatPerson> = convs
        .iter()
        .filter(|row| row.is_group)
        .map(|row| ChatPerson {
            name: row.name.clone(),
            online: false,
            is_group: true,
            uid: row.thread_id.clone(),
        })
        .collect();
    Snapshot {
        convs,
        messages,
        friends,
        groups,
        display_name: None,
        uin: None,
        send2me_id: None,
        session_dir: None,
        has_session: false,
        next_msg_id,
    }
}

/// Optional quote metadata persisted alongside a locally sent reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredQuote {
    pub sender: String,
    pub text: String,
    pub global_id: Option<i64>,
    pub cli_id: Option<i64>,
}

/// Best-effort local insert with optional reply-quote attachment metadata.
pub fn persist_local_message_quote(
    thread_id: &str,
    body: &str,
    _from_me: bool,
    at_ms: i64,
    cli_msg_id: i64,
    quote: Option<&StoredQuote>,
) {
    if cfg!(test) || !live_allowed(thread_id) || body.is_empty() {
        return;
    }
    let Some(dir) = active_data_dir() else {
        return;
    };
    let path = dir.join("store.db");
    if !path.is_file() {
        return;
    }
    let Ok(store) = LocalStore::open(&path) else {
        return;
    };
    let record = MessageRecord {
        message_id: 0,
        thread_id: thread_id.to_owned(),
        sender_id: None,
        cli_msg_id: Some(cli_msg_id),
        global_msg_id: None,
        body: body.to_owned(),
        sent_at: at_ms,
    };
    if let Ok(row) = store.insert_message(&record) {
        if let Some(q) = quote {
            let data = serde_json::json!({
                "kind": "text",
                "quote": {
                    "sender": q.sender,
                    "text": q.text,
                    "gid": q.global_id,
                    "cid": q.cli_id,
                }
            });
            let _ = store.set_attachment(row, &data.to_string());
        }
    }
}

/// Best-effort title update. Does not touch unread or the timestamp.
pub fn persist_thread_title(thread_id: &str, title: &str) {
    if cfg!(test) || !live_allowed(thread_id) {
        return;
    }
    let title = title.trim();
    if title.is_empty() || crate::profiles::is_id_label(thread_id, title) {
        return;
    }
    let Some(dir) = active_data_dir() else {
        return;
    };
    let path = dir.join("store.db");
    if !path.is_file() {
        return;
    }
    let Ok(store) = LocalStore::open(&path) else {
        return;
    };
    let _ = store.set_thread_title(thread_id, title);
}

/// Best-effort local delete by SQLite `message_id`.
pub fn persist_delete_message(message_id: i64) {
    if cfg!(test) || message_id <= 0 {
        return;
    }
    let Some(dir) = active_data_dir() else {
        return;
    };
    let path = dir.join("store.db");
    if !path.is_file() {
        return;
    }
    let Ok(store) = LocalStore::open(&path) else {
        return;
    };
    let _ = store.delete_message(message_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conv_time_rel_follows_zalo_rules() {
        let now = 1_794_400_000_000_i64; // 2026-11-11 ~ 02:26 UTC
        assert_eq!(conv_time_rel(now - 10_000, now), "Vài giây");
        assert_eq!(conv_time_rel(now - 34 * 60_000, now), "34 phút");
        assert_eq!(conv_time_rel(now - 9 * 3_600_000, now), "9 giờ");
        assert_eq!(conv_time_rel(now - 3 * 86_400_000, now), "3 ngày");
        let (_, m, d) = vn_ymd(now - 10 * 86_400_000);
        assert_eq!(
            conv_time_rel(now - 10 * 86_400_000, now),
            format!("{d:02}/{m:02}")
        );
        assert!(conv_time_rel(now - 400 * 86_400_000, now).len() == 8);
    }

    #[test]
    fn preview_ids_never_go_live() {
        assert!(!live_allowed(""));
        assert!(!live_allowed("preview:1"));
        assert!(!live_allowed("1"));
        assert!(!live_allowed("2"));
        assert!(!live_allowed("3"));
        assert!(!live_allowed("4"));
        assert!(live_allowed("g_200000000000000002"));
        assert!(live_allowed("100000000000000004"));
    }

    #[test]
    fn photo_album_groups_one_send_and_keeps_a_later_photo() {
        let photo = |at: i64, layout: i64| {
            let mut msg = ChatMsg::text_msg(
                at as u64,
                1,
                "t".into(),
                true,
                "Bạn".into(),
                String::new(),
                "[Hình ảnh]".into(),
                "00:00".into(),
                at,
                None,
                None,
            );
            msg.kind = MsgKind::Photo;
            msg.group_layout_id = layout;
            msg
        };
        let burst = vec![photo(1_000, 0), photo(1_042, 0), photo(1_083, 0)];
        assert_eq!(photo_album_end(&burst, 0), 3);
        assert_eq!(photo_album_end(&burst, 1), 3);
        let mut later = burst.clone();
        later.push(photo(8_000, 0));
        assert_eq!(photo_album_end(&later, 0), 3);
        assert_eq!(photo_album_end(&later, 3), 4);
        let shared = vec![photo(1_000, 77), photo(9_000, 77)];
        assert_eq!(photo_album_end(&shared, 0), 2);
        let text = ChatMsg::text_msg(
            5,
            1,
            "t".into(),
            true,
            "Bạn".into(),
            String::new(),
            "xin chào".into(),
            "00:00".into(),
            1_050,
            None,
            None,
        );
        let split = vec![photo(1_000, 0), text, photo(1_080, 0)];
        assert_eq!(photo_album_end(&split, 0), 1);
        assert_eq!(photo_album_end(&split, 2), 3);
    }

    #[test]
    fn hash_id_skips_preview_range() {
        let id = hash_id("g_200000000000000002");
        assert_ne!(id, 0);
        assert!(!(1..=4).contains(&id));
        assert_eq!(hash_id("same"), hash_id("same"));
        assert_ne!(hash_id("a"), hash_id("b"));
    }

    #[test]
    fn vn_midnight_unix_is_seven_am() {
        assert_eq!(vn_hm(0), "07:00");
        assert_eq!(day_label(0, 0), "07:00 Hôm nay");
        let snap = load_snapshot();
        assert_eq!(snap.convs.len(), 4);
        assert!(snap.convs.iter().all(|row| !row.live));
        assert!(snap
            .convs
            .iter()
            .all(|row| row.thread_id.starts_with("preview:")));
        assert!(!session_present());
    }

    #[test]
    fn logged_in_shell_without_store_has_no_preview_rows() {
        let dir = std::env::temp_dir().join(format!("zalo-gpui-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session = StoredSession {
            zpw_sek: Some("test-sek".to_owned()),
            display_name: Some("Tester".to_owned()),
            uin: Some("100".to_owned()),
            ..StoredSession::default()
        };
        let snap = snapshot_from_session(&dir, &session);
        assert!(snap.has_session);
        assert!(snap.convs.is_empty());
        assert!(snap.messages.is_empty());
        assert_eq!(snap.session_dir.as_deref(), Some(dir.as_path()));
        assert_eq!(snap.display_name.as_deref(), Some("Tester"));
        assert!(snap
            .convs
            .iter()
            .all(|row| !row.thread_id.starts_with("preview:")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wipe_session_file_removes_only_session_json() {
        let dir = std::env::temp_dir().join(format!("zalo-gpui-wipe-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let session = dir.join("session.json");
        let device = dir.join("device.json");
        std::fs::write(&session, "{}").unwrap();
        std::fs::write(&device, "{}").unwrap();
        wipe_session_file(&dir);
        assert!(!session.is_file());
        assert!(device.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wipe_account_data_clears_account_state_but_keeps_device_and_app_prefs() {
        let dir = std::env::temp_dir().join(format!("zalo-gpui-wipe-acct-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session.json"), "{}").unwrap();
        std::fs::write(dir.join("device.json"), "{\"imei\":\"keep\"}").unwrap();
        std::fs::write(dir.join("store.db"), "sqlite").unwrap();
        std::fs::write(dir.join("store.db-wal"), "wal").unwrap();
        std::fs::write(dir.join("queue-cursors.json"), "{}").unwrap();
        write_last_account_uid(&dir, "12345");
        let mut prefs = crate::prefs::Prefs::load(Some(dir.clone()));
        prefs.set("dark", serde_json::Value::Bool(true));
        prefs.set("hidden", serde_json::Value::Bool(true));
        prefs.set("pinned", serde_json::json!(["111"]));

        wipe_account_data(&dir);

        assert!(dir.join("device.json").is_file());
        assert!(!dir.join("session.json").is_file());
        assert!(!dir.join("store.db").is_file());
        assert!(!dir.join("store.db-wal").is_file());
        assert!(!dir.join("queue-cursors.json").is_file());
        assert!(read_last_account_uid(&dir).is_none());

        let reloaded = crate::prefs::Prefs::load(Some(dir.clone()));
        assert!(reloaded.bool("dark", false));
        assert!(!reloaded.bool("hidden", false));
        assert!(reloaded.json("pinned").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn account_dirs_do_not_share_a_session() {
        let machine = std::env::temp_dir().join(format!(
            "zalo-gpui-acct-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&machine);
        std::fs::create_dir_all(&machine).unwrap();
        let a = account_dir(&machine, "111");
        let b = account_dir(&machine, "222");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("session.json"), "{\"user\":\"a\"}").unwrap();
        std::fs::write(b.join("store.db"), "b-only").unwrap();
        write_current_user(&machine, "111");
        assert_eq!(read_current_user(&machine).as_deref(), Some("111"));
        assert!(a.join("session.json").is_file());
        assert!(!b.join("session.json").is_file());
        detach_account(&a);
        assert!(read_current_user(&machine).is_none());
        assert!(!a.join("session.json").is_file());
        assert_eq!(std::fs::read(b.join("store.db")).unwrap(), b"b-only");
        let _ = std::fs::remove_dir_all(&machine);
    }
}
