//! System-wide shortcuts through `org.freedesktop.portal.GlobalShortcuts`.
//!
//! [`GlobalShortcuts::start`] runs a worker thread that owns one session-bus
//! connection. It registers the host app id, creates a portal session, binds
//! the shortcuts and reports what the desktop accepted. A second thread
//! listens for `Activated`. [`GlobalShortcuts::rebind`] closes the session
//! and binds the new list on a fresh one.
//!
//! Plasma stores `preferred_trigger` only as the default for a new id and
//! asks the user to accept it in a dialog; `BindShortcuts` answers once the
//! dialog closes. It never applies a new `preferred_trigger` to an id it
//! already knows, so callers put the trigger in the id (see [`shortcut_id`])
//! to make a changed combination reach the desktop. A replaced id is removed
//! from `kglobalaccel` so its old keys are not held.
//!
//! A preferred trigger that `kglobalaccel` already gave to another component
//! is left unbound, and this app's old id for it is unregistered. Plasma
//! will not move a known id onto that trigger; it keeps whatever live chord
//! the id already has (on this machine, X-Capture's repeat chord). Re-binding
//! it opens X-Capture's region crosshair. `Activated` for 1500 ms after a
//! bind is ignored so a register-time echo cannot start a region capture.
//!
//! Unit tests cover the pure helpers. They do not open the bus.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Qt modifier bits that `org.kde.KGlobalAccel.action` expects.
const QT_SHIFT: i32 = 0x0200_0000;
const QT_CTRL: i32 = 0x0400_0000;
const QT_ALT: i32 = 0x0800_0000;
const QT_META: i32 = 0x1000_0000;

/// Ignore `Activated` this long after the portal session handle is published.
/// A bind can echo the shortcut that was just registered.
const BIND_ACTIVATION_GRACE: Duration = Duration::from_millis(1500);

struct SessionGate {
    handle: Option<String>,
    suppress_until: Instant,
}

use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::bus::{register_host_portal_app, session};
use crate::error::ShellError;
use crate::screenshot::{
    portal_request_path, PORTAL_PATH, PORTAL_REQUEST_INTERFACE, PORTAL_SERVICE,
};

/// Portal interface for system-wide shortcuts.
pub const PORTAL_GLOBAL_SHORTCUTS_INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// Interface of the session object that `CreateSession` returns.
pub const PORTAL_SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";

/// Plasma's shortcut daemon. The portal backend stores shortcuts there under
/// the app id.
pub const KDE_GLOBALACCEL_SERVICE: &str = "org.kde.kglobalaccel";

/// Host app registry (xdg-desktop-portal 1.19+). Unsandboxed apps call it so
/// the portal knows which app owns the shortcuts.
pub const PORTAL_REGISTRY_INTERFACE: &str = "org.freedesktop.host.portal.Registry";

/// One shortcut to bind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutSpec {
    /// Stable id, reported back in [`ShortcutEvent::Activated`].
    pub id: String,
    /// Text the desktop shows in its shortcut settings.
    pub description: String,
    /// Preferred trigger in the XDG shortcuts format, e.g. `CTRL+ALT+s`.
    pub trigger: String,
}

/// What the worker reports to the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutEvent {
    /// The portal bound the shortcuts: `(id, trigger as the desktop shows it)`.
    /// The trigger is empty when the desktop assigned none.
    Bound(Vec<(String, String)>),
    /// A bound shortcut was pressed.
    Activated(String),
    /// No portal, or it refused. The text is for logs and tooltips.
    Unavailable(String),
}

type Callback = Arc<dyn Fn(ShortcutEvent) + Send + Sync>;

/// Handle to the worker thread. Dropping it ends the worker and the session.
pub struct GlobalShortcuts {
    commands: Sender<Vec<ShortcutSpec>>,
}

impl GlobalShortcuts {
    /// Starts the worker and binds `specs`. `on_event` runs on worker threads.
    pub fn start(
        app_id: &str,
        specs: Vec<ShortcutSpec>,
        on_event: impl Fn(ShortcutEvent) + Send + Sync + 'static,
    ) -> Self {
        let (commands, rx) = mpsc::channel();
        let app_id = app_id.to_owned();
        let callback: Callback = Arc::new(on_event);
        std::thread::spawn(move || run(&app_id, specs, &rx, &callback));
        Self { commands }
    }

    /// Replaces the bound shortcuts with `specs`.
    pub fn rebind(&self, specs: Vec<ShortcutSpec>) {
        let _ = self.commands.send(specs);
    }
}

/// `{slot}-{trigger}` with the trigger lowercased and `+` turned into `-`,
/// e.g. `shot-with-window-ctrl-alt-s`.
#[must_use]
pub fn shortcut_id(slot: &str, trigger: &str) -> String {
    let trigger: String = trigger
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{slot}-{trigger}")
}

/// Slot part of an id made by [`shortcut_id`], given the known slot names.
#[must_use]
pub fn shortcut_slot<'a>(id: &str, slots: &[&'a str]) -> Option<&'a str> {
    slots
        .iter()
        .copied()
        .filter(|slot| id == *slot || id.starts_with(&format!("{slot}-")))
        .max_by_key(|slot| slot.len())
}

fn token(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}_{}_{nanos}", std::process::id())
}

fn bus_error(error: impl std::fmt::Display) -> ShellError {
    ShellError::Bus(error.to_string())
}

fn run(
    app_id: &str,
    first: Vec<ShortcutSpec>,
    rx: &Receiver<Vec<ShortcutSpec>>,
    on_event: &Callback,
) {
    let connection = match session() {
        Ok(connection) => connection,
        Err(error) => {
            on_event(ShortcutEvent::Unavailable(error.to_string()));
            return;
        }
    };
    // Old portals have no registry, and sandboxed apps already have an id.
    register_host_portal_app(&connection, app_id);
    let current = Arc::new(Mutex::new(SessionGate {
        handle: None,
        suppress_until: Instant::now(),
    }));
    if let Err(error) = listen(&connection, &current, on_event) {
        on_event(ShortcutEvent::Unavailable(error.to_string()));
        return;
    }
    let mut specs = first;
    loop {
        let old = current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .handle
            .take();
        if let Some(old) = old {
            close_session(&connection, &old);
        }
        let bindable = shortcuts_free_to_bind(&connection, app_id, &specs);
        match bind(&connection, &bindable) {
            Ok((handle, bound)) => {
                {
                    let mut gate = current.lock().unwrap_or_else(PoisonError::into_inner);
                    // Publish the grace and the handle together. The listener
                    // must not observe a session that is already "live".
                    gate.suppress_until = Instant::now() + BIND_ACTIVATION_GRACE;
                    gate.handle = Some(handle);
                }
                on_event(ShortcutEvent::Bound(bound));
            }
            Err(error) => on_event(ShortcutEvent::Unavailable(error.to_string())),
        }
        let Ok(next) = rx.recv() else {
            break;
        };
        // Several quick edits in a row: only the last list matters.
        let next = rx.try_iter().last().unwrap_or(next);
        for spec in &specs {
            if !next.iter().any(|kept| kept.id == spec.id) {
                forget_kde_shortcut(&connection, app_id, &spec.id);
            }
        }
        specs = next;
    }
    let old = current
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .handle
        .take();
    if let Some(old) = old {
        close_session(&connection, &old);
    }
}

fn listen(
    connection: &zbus::blocking::Connection,
    current: &Arc<Mutex<SessionGate>>,
    on_event: &Callback,
) -> Result<(), ShellError> {
    let proxy = zbus::blocking::Proxy::new(
        connection,
        PORTAL_SERVICE,
        PORTAL_PATH,
        PORTAL_GLOBAL_SHORTCUTS_INTERFACE,
    )
    .map_err(bus_error)?;
    let signals = proxy.receive_signal("Activated").map_err(bus_error)?;
    let current = Arc::clone(current);
    let on_event = Arc::clone(on_event);
    std::thread::spawn(move || {
        for message in signals {
            let Ok((handle, id, _stamp, _options)) =
                message
                    .body()
                    .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>()
            else {
                continue;
            };
            let deliver = {
                let gate = current.lock().unwrap_or_else(PoisonError::into_inner);
                activation_due(
                    gate.handle.as_deref(),
                    handle.as_str(),
                    Instant::now(),
                    gate.suppress_until,
                )
            };
            if deliver {
                on_event(ShortcutEvent::Activated(id));
            }
        }
    });
    Ok(())
}

/// Calls a portal method that answers through a `Request` object and waits
/// for its `Response`. `call` gets the `handle_token` to put in the options.
fn request(
    connection: &zbus::blocking::Connection,
    call: impl FnOnce(&str) -> zbus::Result<zbus::Message>,
) -> Result<HashMap<String, OwnedValue>, ShellError> {
    let unique = connection
        .unique_name()
        .ok_or_else(|| ShellError::Bus("session bus has no unique name".into()))?
        .as_str()
        .to_owned();
    let handle_token = token("zalo_shortcuts");
    let path = portal_request_path(&unique, &handle_token);
    let proxy = zbus::blocking::Proxy::new(
        connection,
        PORTAL_SERVICE,
        path.as_str(),
        PORTAL_REQUEST_INTERFACE,
    )
    .map_err(bus_error)?;
    let mut signals = proxy.receive_signal("Response").map_err(bus_error)?;
    call(&handle_token).map_err(bus_error)?;
    let message = signals
        .next()
        .ok_or_else(|| ShellError::Bus("portal Response stream closed".into()))?;
    let (status, results): (u32, HashMap<String, OwnedValue>) =
        message.body().deserialize().map_err(bus_error)?;
    if status != 0 {
        return Err(ShellError::Bus(format!(
            "portal request refused (status {status})"
        )));
    }
    Ok(results)
}

fn bind(
    connection: &zbus::blocking::Connection,
    specs: &[ShortcutSpec],
) -> Result<(String, Vec<(String, String)>), ShellError> {
    let session_token = token("zalo_shortcuts_session");
    let created = request(connection, |handle_token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(handle_token));
        options.insert("session_handle_token", Value::from(session_token.as_str()));
        connection.call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(PORTAL_GLOBAL_SHORTCUTS_INTERFACE),
            "CreateSession",
            &(options,),
        )
    })?;
    let handle = created
        .get("session_handle")
        .and_then(text_of)
        .ok_or_else(|| ShellError::Bus("portal response missing session_handle".into()))?;
    let session_path = ObjectPath::try_from(handle.as_str()).map_err(bus_error)?;
    let bound = request(connection, |handle_token| {
        let shortcuts: Vec<(&str, HashMap<&str, Value<'_>>)> = specs
            .iter()
            .map(|spec| {
                let mut props: HashMap<&str, Value<'_>> = HashMap::new();
                props.insert("description", Value::from(spec.description.as_str()));
                props.insert("preferred_trigger", Value::from(spec.trigger.as_str()));
                (spec.id.as_str(), props)
            })
            .collect();
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(handle_token));
        connection.call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(PORTAL_GLOBAL_SHORTCUTS_INTERFACE),
            "BindShortcuts",
            &(&session_path, shortcuts, "", options),
        )
    });
    let bound = match bound {
        Ok(bound) => bound,
        Err(error) => {
            close_session(connection, &handle);
            return Err(error);
        }
    };
    let triggers = bound
        .get("shortcuts")
        .map(bound_triggers)
        .unwrap_or_default();
    Ok((handle, triggers))
}

fn close_session(connection: &zbus::blocking::Connection, handle: &str) {
    let _ = connection.call_method(
        Some(PORTAL_SERVICE),
        handle,
        Some(PORTAL_SESSION_INTERFACE),
        "Close",
        &(),
    );
}

/// `true` when this `Activated` belongs to the live session and the bind
/// echo window has passed.
fn activation_due(
    session: Option<&str>,
    event_session: &str,
    now: Instant,
    suppress_until: Instant,
) -> bool {
    session == Some(event_session) && now >= suppress_until
}

/// Specs safe to pass to `BindShortcuts`.
///
/// A preferred trigger owned by another component is removed, and this app's
/// id for it is unregistered so the live chord (not the preferred one) is
/// released. A failed lookup, or a key this mapping cannot encode, keeps the
/// spec: other desktops have no `kglobalaccel`.
fn shortcuts_free_to_bind(
    connection: &zbus::blocking::Connection,
    app_id: &str,
    specs: &[ShortcutSpec],
) -> Vec<ShortcutSpec> {
    let mut kept = Vec::with_capacity(specs.len());
    for spec in specs {
        if let Some(owner) = trigger_owner(connection, &spec.trigger)
            .filter(|owner| owner_blocks_bind(owner, app_id))
        {
            eprintln!(
                "zalo-gpui shortcuts: skip {} ({}); key owned by {owner}",
                spec.id, spec.trigger
            );
            forget_kde_shortcut(connection, app_id, &spec.id);
        } else {
            kept.push(spec.clone());
        }
    }
    kept
}

/// Component unique name from `org.kde.KGlobalAccel.action`.
/// `None` means the chord is free, unmapped, or the daemon is absent.
fn trigger_owner(connection: &zbus::blocking::Connection, trigger: &str) -> Option<String> {
    let key = qt_accel_key(trigger)?;
    let message = connection
        .call_method(
            Some(KDE_GLOBALACCEL_SERVICE),
            "/kglobalaccel",
            Some("org.kde.KGlobalAccel"),
            "action",
            &(key,),
        )
        .ok()?;
    let names: Vec<String> = message.body().deserialize().ok()?;
    names.into_iter().next().filter(|name| !name.is_empty())
}

/// `true` when `owner` is some other `kglobalaccel` component.
fn owner_blocks_bind(owner: &str, app_id: &str) -> bool {
    !owner.is_empty() && owner != app_id
}

/// XDG trigger (`CTRL+ALT+s`) as the Qt key code `action(int)` takes.
/// `None` for chords this mapping does not cover, such as function keys.
fn qt_accel_key(trigger: &str) -> Option<i32> {
    let mut mods = 0i32;
    let mut key = None;
    for part in trigger.split('+').filter(|part| !part.is_empty()) {
        match part.to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" => mods |= QT_CTRL,
            "ALT" => mods |= QT_ALT,
            "SHIFT" => mods |= QT_SHIFT,
            "LOGO" | "META" | "SUPER" | "WIN" => mods |= QT_META,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(xdg_letter_key(other)?);
            }
        }
    }
    Some(mods | key?)
}

fn xdg_letter_key(name: &str) -> Option<i32> {
    let bytes = name.as_bytes();
    if bytes.len() != 1 || !bytes[0].is_ascii_alphanumeric() {
        return None;
    }
    Some(i32::from(bytes[0].to_ascii_uppercase()))
}

/// Plasma keeps every id an app ever bound in `kglobalshortcutsrc`, keys
/// included. Drop an id that a rebind replaced so its old keys are free.
/// Other desktops have no `org.kde.kglobalaccel`; the call then fails quietly.
fn forget_kde_shortcut(connection: &zbus::blocking::Connection, app_id: &str, id: &str) {
    let _ = connection.call_method(
        Some(KDE_GLOBALACCEL_SERVICE),
        "/kglobalaccel",
        Some("org.kde.KGlobalAccel"),
        "unregister",
        &(app_id, id),
    );
}

/// A string or object path inside a variant.
fn text_of(value: &OwnedValue) -> Option<String> {
    match &**value {
        Value::Str(text) => Some(text.as_str().to_owned()),
        Value::ObjectPath(path) => Some(path.as_str().to_owned()),
        _ => None,
    }
}

/// `a(sa{sv})` from a `BindShortcuts` response → `(id, trigger_description)`.
fn bound_triggers(value: &OwnedValue) -> Vec<(String, String)> {
    let Value::Array(items) = &**value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let Value::Structure(fields) = item else {
                return None;
            };
            let [Value::Str(id), Value::Dict(props)] = fields.fields() else {
                return None;
            };
            let trigger = props
                .get::<&str, Value<'_>>(&"trigger_description")
                .ok()
                .flatten()
                .and_then(|value| match value {
                    Value::Str(text) => Some(text.as_str().to_owned()),
                    _ => None,
                })
                .unwrap_or_default();
            Some((id.as_str().to_owned(), trigger))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_carry_the_trigger_and_map_back_to_slots() {
        let id = shortcut_id("shot-with-window", "CTRL+ALT+s");
        assert_eq!(id, "shot-with-window-ctrl-alt-s");
        let slots = ["shot-with-window", "shot-with-window-hidden"];
        assert_eq!(shortcut_slot(&id, &slots), Some("shot-with-window"));
        let hidden = shortcut_id("shot-with-window-hidden", "CTRL+SHIFT+F5");
        assert_eq!(hidden, "shot-with-window-hidden-ctrl-shift-f5");
        assert_eq!(
            shortcut_slot(&hidden, &slots),
            Some("shot-with-window-hidden")
        );
        assert_eq!(shortcut_slot("other-ctrl-a", &slots), None);
    }

    #[test]
    fn reads_session_handles_as_string_or_path() {
        let text =
            OwnedValue::try_from(Value::from("/org/freedesktop/portal/desktop/session/1_2/t"))
                .expect("str");
        assert_eq!(
            text_of(&text).as_deref(),
            Some("/org/freedesktop/portal/desktop/session/1_2/t")
        );
        let path = OwnedValue::try_from(Value::from(
            ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_2/t").expect("path"),
        ))
        .expect("path value");
        assert!(text_of(&path).is_some());
        assert!(text_of(&OwnedValue::from(3_u32)).is_none());
    }

    #[test]
    fn qt_key_codes_match_kglobalaccel() {
        assert_eq!(
            qt_accel_key("CTRL+ALT+s"),
            Some(QT_CTRL | QT_ALT | i32::from(b'S'))
        );
        assert_eq!(qt_accel_key("CTRL+ALT+s"), Some(201_326_675));
        assert_eq!(qt_accel_key("CTRL+ALT+SHIFT+s"), Some(234_881_107));
        assert_eq!(qt_accel_key("ctrl+alt+shift+b"), Some(234_881_090));
        assert_eq!(qt_accel_key("CTRL+SHIFT+F5"), None);
        assert_eq!(qt_accel_key("CTRL+ALT"), None);
    }

    #[test]
    fn another_component_blocks_the_bind() {
        assert!(owner_blocks_bind("x-capture", "zalo-gpui"));
        assert!(!owner_blocks_bind("zalo-gpui", "zalo-gpui"));
        assert!(!owner_blocks_bind("", "zalo-gpui"));
    }

    #[test]
    fn activation_waits_out_the_bind_echo() {
        let now = Instant::now();
        let session = "/org/freedesktop/portal/desktop/session/1/s";
        assert!(!activation_due(None, session, now, now));
        assert!(!activation_due(
            Some(session),
            session,
            now,
            now + BIND_ACTIVATION_GRACE
        ));
        assert!(!activation_due(Some("/other"), session, now, now));
        assert!(activation_due(
            Some(session),
            session,
            now + BIND_ACTIVATION_GRACE,
            now
        ));
    }
}
