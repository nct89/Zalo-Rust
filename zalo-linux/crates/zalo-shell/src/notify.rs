//! `org.freedesktop.Notifications.Notify`.
//!
//! Building a notice does not open the bus. [`notify_desktop`] does, and it
//! returns [`ShellError::Bus`] when the session has no notification service.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, Once};

use zbus::zvariant::Value;

use crate::bus::{session, NOTIFICATIONS_PATH, NOTIFICATIONS_SERVICE};
use crate::error::ShellError;
use crate::tray::{active_app_id, record_tray_event, TrayEvent};

static TRACKED_NOTIFICATIONS: Mutex<VecDeque<u32>> = Mutex::new(VecDeque::new());
static LISTENER_STARTED: Once = Once::new();
const MAX_TRACKED_NOTIFS: usize = 32;

pub(crate) fn record_notification_id(id: u32) {
    if id == 0 {
        return;
    }
    if let Ok(mut ids) = TRACKED_NOTIFICATIONS.lock() {
        if !ids.contains(&id) {
            if ids.len() >= MAX_TRACKED_NOTIFS {
                ids.pop_front();
            }
            ids.push_back(id);
        }
    }
}

pub(crate) fn claim_notification_action(id: u32, action_key: &str) -> bool {
    if id == 0 || !matches!(action_key, "default" | "open") {
        return false;
    }
    TRACKED_NOTIFICATIONS
        .lock()
        .map(|mut ids| {
            let Some(idx) = ids.iter().position(|existing| *existing == id) else {
                return false;
            };
            ids.remove(idx);
            true
        })
        .unwrap_or(false)
}

/// Listen for `org.freedesktop.Notifications.ActionInvoked` on the session bus
/// so clicking a notification reopens/focuses Zalo even on tray-less desktops.
pub(crate) fn ensure_notification_action_listener() {
    LISTENER_STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("zalo-notify-actions".into())
            .spawn(listen_notification_actions);
    });
}

fn listen_notification_actions() {
    let Ok(connection) = session() else {
        return;
    };
    let Ok(proxy) = zbus::blocking::Proxy::new(
        &connection,
        NOTIFICATIONS_SERVICE,
        NOTIFICATIONS_PATH,
        NOTIFICATIONS_SERVICE,
    ) else {
        return;
    };
    let Ok(signals) = proxy.receive_signal("ActionInvoked") else {
        return;
    };
    for msg in signals {
        if let Ok((id, action_key)) = msg.body().deserialize::<(u32, String)>() {
            if claim_notification_action(id, &action_key) {
                record_tray_event(TrayEvent::Activate);
            }
        }
    }
}

/// Text for one desktop notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopNotice {
    /// `app_name` argument. This client uses `Zalo`.
    pub app_name: String,
    /// Notification title.
    pub summary: String,
    /// Notification body.
    pub body: String,
    /// Timeout in milliseconds. `5000` is the unread notice.
    pub expire_ms: i32,
}

/// Arguments of `Notify` that do not depend on the bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotifyCall {
    /// Application name.
    pub app_name: String,
    /// `replaces_id`. Zero asks the server to allocate an id.
    pub replaces_id: u32,
    /// Icon name or path. Empty uses the server default.
    pub app_icon: String,
    /// Title.
    pub summary: String,
    /// Body.
    pub body: String,
    /// Timeout in milliseconds.
    pub expire_ms: i32,
}

/// Notice for a positive unread count. Zero returns `None`, so a launch does not pop a banner.
pub fn unread_notice(count: u32) -> Option<DesktopNotice> {
    if count == 0 {
        return None;
    }
    let body = if count > 99 {
        "99+ tin chưa đọc".to_owned()
    } else {
        format!("{count} tin chưa đọc")
    };
    Some(DesktopNotice {
        app_name: "Zalo".to_owned(),
        summary: "Zalo".to_owned(),
        body,
        expire_ms: 5000,
    })
}

/// Map a notice onto the `Notify` arguments.
pub fn notify_call(notice: &DesktopNotice) -> NotifyCall {
    NotifyCall {
        app_name: notice.app_name.clone(),
        replaces_id: 0,
        app_icon: String::new(),
        summary: notice.summary.clone(),
        body: notice.body.clone(),
        expire_ms: notice.expire_ms,
    }
}

/// Send `notice` on the session bus. The returned id comes from the server.
pub fn notify_desktop(notice: &DesktopNotice) -> Result<u32, ShellError> {
    ensure_notification_action_listener();
    let call = notify_call(notice);
    let desktop_entry = active_app_id();
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("desktop-entry", Value::from(desktop_entry.as_str()));
    let actions = ["default", "Mở Zalo"];
    let connection = session()?;
    let reply = connection
        .call_method(
            Some(NOTIFICATIONS_SERVICE),
            NOTIFICATIONS_PATH,
            Some(NOTIFICATIONS_SERVICE),
            "Notify",
            &(
                call.app_name.as_str(),
                call.replaces_id,
                call.app_icon.as_str(),
                call.summary.as_str(),
                call.body.as_str(),
                &actions[..],
                &hints,
                call.expire_ms,
            ),
        )
        .map_err(|error| ShellError::Bus(error.to_string()))?;
    let id: u32 = reply
        .body()
        .deserialize()
        .map_err(|error| ShellError::Bus(error.to_string()))?;
    record_notification_id(id);
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_unread_does_not_build_a_notice() {
        assert_eq!(unread_notice(0), None);
        let notice = unread_notice(2).unwrap();
        let call = notify_call(&notice);
        assert_eq!(call.app_name, "Zalo");
        assert_eq!(call.replaces_id, 0);
        assert_eq!(call.app_icon, "");
        assert_eq!(call.body, "2 tin chưa đọc");
        assert_eq!(call.expire_ms, 5000);
        assert_eq!(unread_notice(100).unwrap().body, "99+ tin chưa đọc");
        assert_eq!(NOTIFICATIONS_SERVICE, "org.freedesktop.Notifications");

        record_notification_id(77);
        assert!(!claim_notification_action(999, "default"));
        assert!(!claim_notification_action(77, "dismiss"));
        assert!(claim_notification_action(77, "default"));
        assert!(!claim_notification_action(77, "default"));
    }
}
