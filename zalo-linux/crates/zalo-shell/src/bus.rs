//! Session-bus helpers. Nothing in the unit tests opens the bus.

use std::collections::HashMap;

use zbus::zvariant::Value;

use crate::error::ShellError;
use crate::screenshot::{PORTAL_PATH, PORTAL_SERVICE};
use crate::shortcuts::PORTAL_REGISTRY_INTERFACE;

/// Well-known name from `plugins/tray-host`.
pub const STATUS_NOTIFIER_WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// Freedesktop notification service.
pub const NOTIFICATIONS_SERVICE: &str = "org.freedesktop.Notifications";

/// Object path of [`NOTIFICATIONS_SERVICE`].
pub const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

pub(crate) fn session() -> Result<zbus::blocking::Connection, ShellError> {
    zbus::blocking::connection::Builder::session()
        .and_then(|builder| builder.build())
        .map_err(|error| ShellError::Bus(error.to_string()))
}

pub(crate) fn name_has_owner(connection: &zbus::blocking::Connection, name: &str) -> bool {
    let Ok(reply) = connection.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "NameHasOwner",
        &name,
    ) else {
        return false;
    };
    reply.body().deserialize().unwrap_or(false)
}

/// Register an unsandboxed desktop app id with xdg-desktop-portal >= 1.19.
pub(crate) fn register_host_portal_app(connection: &zbus::blocking::Connection, app_id: &str) {
    let _ = connection.call_method(
        Some(PORTAL_SERVICE),
        PORTAL_PATH,
        Some(PORTAL_REGISTRY_INTERFACE),
        "Register",
        &(app_id, HashMap::<&str, Value<'_>>::new()),
    );
}

/// `NameHasOwner` for [`STATUS_NOTIFIER_WATCHER`].
///
/// A missing bus or a timeout is `false`. Stock GNOME has no watcher; the tray
/// stays unregistered in that case so the window is not hidden behind it.
pub fn watcher_present() -> bool {
    let Ok(connection) = session() else {
        return false;
    };
    name_has_owner(&connection, STATUS_NOTIFIER_WATCHER)
}
