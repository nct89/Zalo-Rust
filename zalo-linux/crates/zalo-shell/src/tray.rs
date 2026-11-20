//! Desktop system tray (`StatusNotifierItem` + `com.canonical.dbusmenu`) with
//! automatic watcher reconnect and optional X11 XEmbed fallback.
//!
//! Primary backend is pure-Rust `org.kde.StatusNotifierItem` +
//! `com.canonical.dbusmenu` over `zbus` (with live `NameOwnerChanged` tracking so
//! autostart races and panel restarts recover automatically). When built with
//! `--features tray-xembed` and no StatusNotifier watcher is present on X11,
//! `xembed` docks a native `_NET_SYSTEM_TRAY` icon into i3bar, polybar, tint2,
//! Openbox, or legacy notification areas.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::{ObjectPath, OwnedValue, Str, Value};

use crate::badge::{
    default_base_png, normalize_count, png_to_argb, render_tray_badge, zalo_icon_png, BASE_ICON_PX,
};
use crate::bus::{name_has_owner, session, STATUS_NOTIFIER_WATCHER};
use crate::error::ShellError;
use crate::launcher::{desktop_files_from_env, publish_unity_badge};
use crate::notify::{ensure_notification_action_listener, notify_desktop, unread_notice};

const ITEM_PATH: &str = "/StatusNotifierItem";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const MENU_PATH: &str = "/StatusNotifierItem/Menu";

const MENU_ID_OPEN: i32 = 1;
const MENU_ID_HIDE: i32 = 2;
const MENU_ID_QUIT: i32 = 3;

/// Active tray backend currently visible to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayBackend {
    /// No tray host registered (e.g. stock GNOME Wayland without extension).
    None,
    /// `org.kde.StatusNotifierItem` + `com.canonical.dbusmenu`.
    StatusNotifier,
    /// Legacy X11 `_NET_SYSTEM_TRAY` XEmbed icon (`--features tray-xembed`).
    XEmbed,
}

#[derive(Clone)]
struct TrayState {
    base_png: Vec<u8>,
    count: u32,
    app_id: String,
    menu_revision: u32,
}

struct StatusItem {
    state: Arc<Mutex<TrayState>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl StatusItem {
    /// `Application`. The host uses this to group the icon.
    #[zbus(property)]
    fn category(&self) -> &str {
        "Application"
    }

    /// Stable id. Distinct from the installed Electron Zalo indicator.
    #[zbus(property)]
    fn id(&self) -> &str {
        "zalo-linux"
    }

    /// Tooltip title, including the unread count when it is positive.
    #[zbus(property)]
    fn title(&self) -> String {
        let count = self.state.lock().map(|state| state.count).unwrap_or(0);
        if count == 0 {
            "Zalo".to_owned()
        } else {
            format!("Zalo ({count})")
        }
    }

    /// `Active`. The unread count is painted on the pixmap.
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    /// Unused window id. Zero means the item is not tied to an X window.
    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    /// Theme icon fallback matching the installed hicolor icon for the GUI binary.
    #[zbus(property)]
    fn icon_name(&self) -> String {
        self.state
            .lock()
            .map(|state| state.app_id.clone())
            .unwrap_or_else(|_| crate::GPUI_APP_ID.to_owned())
    }

    /// Empty custom theme path; icons live in standard `hicolor` or `icon_pixmap`.
    #[zbus(property)]
    fn icon_theme_path(&self) -> &str {
        ""
    }

    /// Primary click invokes [`Self::activate`] rather than popping a menu.
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    /// DBus menu shown on right-click. Left click still calls [`Self::activate`].
    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'static> {
        ObjectPath::from_static_str(MENU_PATH).expect("menu path")
    }

    /// ARGB32 pixmap, network byte order, as `(width, height, bytes)`.
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        let png = render_tray_badge(&state.base_png, state.count)
            .unwrap_or_else(|_| state.base_png.clone());
        match png_to_argb(&png) {
            Ok((width, height, data)) => vec![(width, height, data)],
            Err(_) => Vec::new(),
        }
    }

    /// Primary click. Queues [`TrayEvent::Activate`] for the GUI loop.
    fn activate(&self, _x: i32, _y: i32) {
        record_tray_event(TrayEvent::Activate);
    }

    /// Secondary click. Queues [`TrayEvent::SecondaryActivate`] for the GUI loop.
    fn secondary_activate(&self, _x: i32, _y: i32) {
        record_tray_event(TrayEvent::SecondaryActivate);
    }

    /// Context-menu request from hosts that do not render `com.canonical.dbusmenu`.
    fn context_menu(&self, _x: i32, _y: i32) {
        record_tray_event(TrayEvent::ContextMenu);
    }

    /// Scroll. The unread count is not changed from the tray.
    fn scroll(&self, _delta: i32, _orientation: &str) {}
}

/// Interaction emitted by the tray icon, notification click, or instance IPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// Primary click (`Activate`) or notification/Quicklist open — show main window.
    Activate,
    /// Secondary / middle click (`SecondaryActivate`) — show main window.
    SecondaryActivate,
    /// Explicit context-menu trigger (`ContextMenu` / XEmbed right click).
    ContextMenu,
    /// Tray menu "Hide Zalo" — hide/minimize the main window to the tray.
    Hide,
    /// Toggle message notifications (`notifyOff`) from Dock Quicklist CLI.
    ToggleMute,
    /// Tray menu "Quit" / Quicklist `--quit` — leave the process.
    Quit,
}

/// Right-click menu (`com.canonical.dbusmenu`) for the tray icon.
struct DbusMenu {
    state: Arc<Mutex<TrayState>>,
}

impl DbusMenu {
    fn snapshot(&self) -> u32 {
        self.state.lock().map(|s| s.menu_revision).unwrap_or(1)
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    /// Protocol version 3.
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    /// `normal` while the menu can be shown.
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    /// Left to right.
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    /// Icons are resolved from the system icon theme (`hicolor`).
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<&str> {
        Vec::new()
    }

    /// The menu tree. `parent_id` 0 is the root.
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        _property_names: Vec<&str>,
    ) -> (u32, MenuNode) {
        let revision = self.snapshot();
        (revision, menu_node(parent_id, recursion_depth))
    }

    /// Properties of the requested items.
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<&str>,
    ) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        ids.into_iter()
            .filter_map(|id| menu_props(id).map(|props| (id, props)))
            .collect()
    }

    /// One property of one item.
    fn get_property(&self, id: i32, name: &str) -> OwnedValue {
        menu_props(id)
            .and_then(|mut props| props.remove(name))
            .unwrap_or_else(|| owned_text(""))
    }

    /// Click event handler. Some hosts call this; XFCE's libdbusmenu calls
    /// [`Self::event_group`] instead and ignores a missing `Event`.
    fn event(&self, id: i32, event_id: &str, _data: OwnedValue, _timestamp: u32) {
        let _ = dispatch_menu_event(id, event_id);
    }

    /// Batched clicks. libdbusmenu 18.10 (XFCE Status Tray) sends `clicked`
    /// here. Without this method the call returns `UnknownMethod` and "Quit"
    /// does nothing. Unknown ids are returned so the host can drop them.
    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        let mut errors = Vec::new();
        for (id, event_id, _data, _timestamp) in events {
            if !dispatch_menu_event(id, &event_id) {
                errors.push(id);
            }
        }
        errors
    }

    /// Menu state stays updated eagerly on property changes.
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    /// Same answer as [`Self::about_to_show`], for hosts that batch the call.
    /// `updatesNeeded` is empty. Unknown ids go in `idErrors`.
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        let errors = ids
            .into_iter()
            .filter(|id| *id != 0 && menu_props(*id).is_none())
            .collect();
        (Vec::new(), errors)
    }
}

/// `true` when `id` is a real item. A click on it is queued for the GUI loop.
fn dispatch_menu_event(id: i32, event_id: &str) -> bool {
    if menu_props(id).is_none() {
        return false;
    }
    if event_id != "clicked" {
        return true;
    }
    let event = match id {
        MENU_ID_OPEN => TrayEvent::Activate,
        MENU_ID_HIDE => TrayEvent::Hide,
        MENU_ID_QUIT => TrayEvent::Quit,
        _ => return true,
    };
    record_tray_event(event);
    true
}

/// One dbusmenu node: id, properties, children.
type MenuNode = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn menu_node(parent_id: i32, recursion_depth: i32) -> MenuNode {
    if parent_id == 0 {
        let mut props = HashMap::new();
        props.insert("children-display".to_owned(), owned_text("submenu"));
        let children = if recursion_depth == 0 {
            Vec::new()
        } else {
            let child_depth = if recursion_depth > 0 {
                recursion_depth - 1
            } else {
                -1
            };
            [MENU_ID_OPEN, MENU_ID_HIDE, MENU_ID_QUIT]
                .into_iter()
                .map(|id| {
                    let (item_id, props, kids) = menu_node(id, child_depth);
                    owned_node(item_id, props, kids)
                })
                .collect()
        };
        return (0, props, children);
    }
    (
        parent_id,
        menu_props(parent_id).unwrap_or_default(),
        Vec::new(),
    )
}

fn menu_props(id: i32) -> Option<HashMap<String, OwnedValue>> {
    let label = match id {
        MENU_ID_OPEN => "Mở Zalo",
        MENU_ID_HIDE => "Ẩn Zalo",
        MENU_ID_QUIT => "Thoát",
        _ => return None,
    };
    let mut props = HashMap::new();
    props.insert("type".to_owned(), owned_text("standard"));
    props.insert("enabled".to_owned(), OwnedValue::from(true));
    props.insert("visible".to_owned(), OwnedValue::from(true));
    props.insert("label".to_owned(), owned_text(label));
    Some(props)
}

/// A D-Bus string. `OwnedValue` has no `From<&str>`.
fn owned_text(text: &str) -> OwnedValue {
    OwnedValue::from(Str::from(text))
}

/// One menu row wrapped as a variant. Children of a layout node are type `av`.
fn owned_node(
    id: i32,
    props: HashMap<String, OwnedValue>,
    children: Vec<OwnedValue>,
) -> OwnedValue {
    OwnedValue::try_from(Value::from((id, props, children))).expect("menu node")
}

#[derive(Clone, Copy)]
enum TrayUpdate {
    Count(u32),
}

static UPDATES: Mutex<Option<mpsc::Sender<TrayUpdate>>> = Mutex::new(None);
static TRAY_EVENTS: Mutex<Vec<TrayEvent>> = Mutex::new(Vec::new());
#[cfg(test)]
pub(crate) static TEST_EVENT_LOCK: Mutex<()> = Mutex::new(());
static SNI_ACTIVE: AtomicBool = AtomicBool::new(false);
static XEMBED_ACTIVE: AtomicBool = AtomicBool::new(false);
static PREFERRED_APP_ID: Mutex<Option<String>> = Mutex::new(None);

#[cfg(feature = "tray-xembed")]
static XEMBED_CTRL: Mutex<Option<Arc<crate::xembed::XEmbedControl>>> = Mutex::new(None);

/// Active desktop app id used for notifications and tray theme fallback.
#[must_use]
pub fn active_app_id() -> String {
    PREFERRED_APP_ID
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| crate::GPUI_APP_ID.to_owned())
}

/// Record a tray interaction for the GUI event loop to drain.
pub fn record_tray_event(event: TrayEvent) {
    if let Ok(mut queue) = TRAY_EVENTS.lock() {
        if queue.len() < 64 {
            queue.push(event);
        }
    }
}

/// Drain all pending tray interactions recorded since the last poll.
#[must_use]
pub fn take_tray_events() -> Vec<TrayEvent> {
    let Ok(mut queue) = TRAY_EVENTS.lock() else {
        return Vec::new();
    };
    std::mem::take(&mut *queue)
}

/// Start the tray on a background thread using default `app_id`.
///
/// Safe to call even if no tray host exists yet: listens for
/// `NameOwnerChanged` on `org.kde.StatusNotifierWatcher` and falls back to
/// X11 XEmbed when compiled with `--features tray-xembed`.
pub fn install_desktop_shell() {
    install_desktop_shell_for(crate::APP_ID);
}

/// Start the tray background worker for a specific desktop `app_id`
/// (`zalo-gpui` or `zalo-gui`).
pub fn install_desktop_shell_for(app_id: &str) {
    if let Ok(mut guard) = PREFERRED_APP_ID.lock() {
        *guard = Some(app_id.to_owned());
    }
    let app_id = app_id.to_owned();
    let _ = std::thread::Builder::new()
        .name("zalo-tray".into())
        .spawn(move || run_tray(app_id));
}

fn ensure_xembed_started(state: &Arc<Mutex<TrayState>>) {
    #[cfg(feature = "tray-xembed")]
    {
        if XEMBED_ACTIVE.load(Ordering::Acquire) {
            return;
        }
        let Ok(guard) = state.lock() else {
            return;
        };
        let base_png = guard.base_png.clone();
        let count = guard.count;
        drop(guard);
        let on_active = Arc::new(|active| {
            XEMBED_ACTIVE.store(active, Ordering::Release);
        });
        if let Some(ctrl) = crate::xembed::start_xembed_tray(base_png, count, on_active) {
            if let Ok(mut slot) = XEMBED_CTRL.lock() {
                *slot = Some(ctrl);
            }
        }
    }
    #[cfg(not(feature = "tray-xembed"))]
    {
        let _ = state;
    }
}

fn stop_xembed() {
    #[cfg(feature = "tray-xembed")]
    {
        if let Ok(mut slot) = XEMBED_CTRL.lock() {
            if let Some(ctrl) = slot.take() {
                ctrl.stop();
            }
        }
        XEMBED_ACTIVE.store(false, Ordering::Release);
    }
}

fn update_xembed_count(count: u32) {
    #[cfg(feature = "tray-xembed")]
    {
        if let Ok(slot) = XEMBED_CTRL.lock() {
            if let Some(ctrl) = slot.as_ref() {
                ctrl.update_count(count);
            }
        }
    }
    #[cfg(not(feature = "tray-xembed"))]
    {
        let _ = count;
    }
}

fn run_tray(app_id: String) {
    ensure_notification_action_listener();
    let Ok(base_png) = zalo_icon_png(BASE_ICON_PX).or_else(|_| default_base_png(BASE_ICON_PX))
    else {
        return;
    };
    let (sender, receiver) = mpsc::channel();
    let state = Arc::new(Mutex::new(TrayState {
        base_png,
        count: 0,
        app_id,
        menu_revision: 1,
    }));
    if let Ok(mut slot) = UPDATES.lock() {
        *slot = Some(sender);
    }

    let item = StatusItem {
        state: Arc::clone(&state),
    };
    let menu = DbusMenu {
        state: Arc::clone(&state),
    };
    let bus_name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    let Ok(connection) = serve_item(&bus_name, item, menu) else {
        // Session D-Bus unavailable: still allow X11 XEmbed fallback if present.
        ensure_xembed_started(&state);
        drain_update_loop(&state, None, receiver);
        return;
    };

    if name_has_owner(&connection, STATUS_NOTIFIER_WATCHER)
        && register_item(&connection, &bus_name).is_ok()
    {
        SNI_ACTIVE.store(true, Ordering::Release);
    } else {
        // Give boot/panel startup a tiny grace window before activating XEmbed
        // so KDE/Wayland autostart doesn't flicker through xembedsniproxy.
        std::thread::sleep(Duration::from_millis(300));
        if name_has_owner(&connection, STATUS_NOTIFIER_WATCHER)
            && register_item(&connection, &bus_name).is_ok()
        {
            SNI_ACTIVE.store(true, Ordering::Release);
        } else {
            ensure_xembed_started(&state);
        }
    }

    let watch_state = Arc::clone(&state);
    let watch_bus_name = bus_name;
    let _ = std::thread::Builder::new()
        .name("zalo-tray-watch".into())
        .spawn(move || monitor_watcher(watch_state, watch_bus_name));

    drain_update_loop(&state, Some(&connection), receiver);
}

fn drain_update_loop(
    state: &Arc<Mutex<TrayState>>,
    connection: Option<&zbus::blocking::Connection>,
    receiver: mpsc::Receiver<TrayUpdate>,
) {
    while let Ok(update) = receiver.recv() {
        match update {
            TrayUpdate::Count(count) => {
                if let Ok(mut tray) = state.lock() {
                    tray.count = count;
                }
                update_xembed_count(count);
                if SNI_ACTIVE.load(Ordering::Acquire) {
                    if let Some(conn) = connection {
                        let _ = conn.emit_signal(
                            None::<&str>,
                            ITEM_PATH,
                            ITEM_INTERFACE,
                            "NewIcon",
                            &(),
                        );
                        let _ = conn.emit_signal(
                            None::<&str>,
                            ITEM_PATH,
                            ITEM_INTERFACE,
                            "NewTitle",
                            &(),
                        );
                    }
                }
            }
        }
    }
}

fn monitor_watcher(state: Arc<Mutex<TrayState>>, item_bus_name: String) {
    let Ok(conn) = session() else {
        return;
    };
    let Ok(dbus_proxy) = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    ) else {
        return;
    };
    let Ok(signals) = dbus_proxy.receive_signal("NameOwnerChanged") else {
        return;
    };
    for msg in signals {
        let Ok((name, _old_owner, new_owner)) =
            msg.body().deserialize::<(String, String, String)>()
        else {
            continue;
        };
        if name != STATUS_NOTIFIER_WATCHER {
            continue;
        }
        if new_owner.is_empty() {
            SNI_ACTIVE.store(false, Ordering::Release);
            ensure_xembed_started(&state);
        } else if register_item(&conn, &item_bus_name).is_ok() {
            stop_xembed();
            SNI_ACTIVE.store(true, Ordering::Release);
        }
    }
}

fn serve_item(
    bus_name: &str,
    item: StatusItem,
    menu: DbusMenu,
) -> Result<zbus::blocking::Connection, ShellError> {
    zbus::blocking::connection::Builder::session()
        .map_err(|error| ShellError::Bus(error.to_string()))?
        .name(bus_name)
        .map_err(|error| ShellError::Bus(error.to_string()))?
        .serve_at(ITEM_PATH, item)
        .map_err(|error| ShellError::Bus(error.to_string()))?
        .serve_at(MENU_PATH, menu)
        .map_err(|error| ShellError::Bus(error.to_string()))?
        .build()
        .map_err(|error| ShellError::Bus(error.to_string()))
}

fn register_item(
    connection: &zbus::blocking::Connection,
    bus_name: &str,
) -> Result<(), ShellError> {
    connection
        .call_method(
            Some(STATUS_NOTIFIER_WATCHER),
            "/StatusNotifierWatcher",
            Some(STATUS_NOTIFIER_WATCHER),
            "RegisterStatusNotifierItem",
            &bus_name,
        )
        .map_err(|error| ShellError::Bus(error.to_string()))?;
    Ok(())
}

fn send_update(update: TrayUpdate) {
    let sender = UPDATES.lock().ok().and_then(|slot| slot.clone());
    if let Some(sender) = sender {
        let _ = sender.send(update);
    }
}

/// Compatibility hook retained for settings/CLI callers; the tray menu itself stays minimal like native Zalo.
pub fn set_tray_muted(_muted: bool) {}

/// Currently active tray backend.
#[must_use]
pub fn tray_backend() -> TrayBackend {
    if SNI_ACTIVE.load(Ordering::Acquire) {
        TrayBackend::StatusNotifier
    } else if XEMBED_ACTIVE.load(Ordering::Acquire) {
        TrayBackend::XEmbed
    } else {
        TrayBackend::None
    }
}

/// True when any tray backend (SNI or X11 XEmbed) is active.
#[must_use]
pub fn tray_active() -> bool {
    tray_backend() != TrayBackend::None
}

/// Wait briefly for a tray backend to finish connecting (helpful during login autostart).
#[must_use]
pub fn wait_for_tray_ready(timeout: Duration) -> bool {
    let start = Instant::now();
    while !tray_active() && start.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(50));
    }
    tray_active()
}

/// Tray pixmap + Unity launcher count only (no desktop notification).
pub fn set_unread_badge(raw_count: i64) {
    let count = normalize_count(raw_count);
    send_update(TrayUpdate::Count(count));
    for desktop_file in desktop_files_from_env() {
        let _ = publish_unity_badge(&desktop_file, i64::from(count));
    }
}

/// Update the tray pixmap, emit the Unity count, and notify when `count` is positive.
///
/// Count 0 clears the badge and does not call `Notify`. The bus calls return
/// [`ShellError::Bus`] when the session bus is down. Unit tests do not call this.
pub fn publish_unread(raw_count: i64) -> Result<(), ShellError> {
    let count = normalize_count(raw_count);
    send_update(TrayUpdate::Count(count));
    let mut first_error = None;
    for desktop_file in desktop_files_from_env() {
        if let Err(error) = publish_unity_badge(&desktop_file, i64::from(count)) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    if let Some(notice) = unread_notice(count) {
        notify_desktop(&notice)?;
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_event_queue_records_and_drains_in_order() {
        let _guard = TEST_EVENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_tray_events();
        record_tray_event(TrayEvent::Activate);
        record_tray_event(TrayEvent::ContextMenu);
        record_tray_event(TrayEvent::ToggleMute);
        assert_eq!(
            take_tray_events(),
            vec![
                TrayEvent::Activate,
                TrayEvent::ContextMenu,
                TrayEvent::ToggleMute
            ]
        );
        assert!(take_tray_events().is_empty());
    }

    #[test]
    fn tray_menu_offers_open_hide_and_quit() {
        let _guard = TEST_EVENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_id, _props, children) = menu_node(0, -1);
        assert_eq!(children.len(), 3);
        let labels: Vec<String> = [MENU_ID_OPEN, MENU_ID_HIDE, MENU_ID_QUIT]
            .into_iter()
            .filter_map(|id| {
                let mut props = menu_props(id)?;
                String::try_from(props.remove("label")?).ok()
            })
            .collect();
        assert_eq!(labels, ["Mở Zalo", "Ẩn Zalo", "Thoát"]);

        let shallow = menu_node(0, 0);
        assert!(shallow.2.is_empty());

        let menu = DbusMenu {
            state: Arc::new(Mutex::new(TrayState {
                base_png: Vec::new(),
                count: 0,
                app_id: crate::GPUI_APP_ID.to_owned(),
                menu_revision: 1,
            })),
        };
        let _ = take_tray_events();
        menu.event(MENU_ID_OPEN, "clicked", owned_text(""), 0);
        menu.event(MENU_ID_HIDE, "clicked", owned_text(""), 0);
        menu.event(MENU_ID_QUIT, "clicked", owned_text(""), 0);
        assert_eq!(
            take_tray_events(),
            vec![TrayEvent::Activate, TrayEvent::Hide, TrayEvent::Quit]
        );
    }

    #[test]
    fn tray_menu_event_group_quits_like_the_taskbar() {
        let _guard = TEST_EVENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let menu = DbusMenu {
            state: Arc::new(Mutex::new(TrayState {
                base_png: Vec::new(),
                count: 0,
                app_id: crate::GPUI_APP_ID.to_owned(),
                menu_revision: 1,
            })),
        };
        let _ = take_tray_events();
        let unknown = menu.event_group(vec![
            (MENU_ID_OPEN, "hovered".into(), owned_text(""), 1),
            (MENU_ID_QUIT, "clicked".into(), owned_text(""), 2),
            (99, "clicked".into(), owned_text(""), 3),
        ]);
        assert_eq!(unknown, vec![99]);
        assert_eq!(take_tray_events(), vec![TrayEvent::Quit]);
        let (updates, errors) = menu.about_to_show_group(vec![0, MENU_ID_QUIT, 99]);
        assert!(updates.is_empty());
        assert_eq!(errors, vec![99]);
    }
}
