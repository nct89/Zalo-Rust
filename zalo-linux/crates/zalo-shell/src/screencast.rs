//! Wayland screen sharing via `org.freedesktop.portal.ScreenCast` (XDG Desktop
//! Portal) returning a PipeWire remote file descriptor and stream `node_id`.
//!
//! Pure safe Rust over `zbus`; works across GNOME, KDE Plasma, Hyprland, Sway,
//! and Flatpak sandboxes without linking `libpipewire` at compile time.

use std::collections::HashMap;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use zbus::zvariant::{ObjectPath, OwnedFd as ZbusOwnedFd, OwnedValue, Value};

use crate::bus::session;
use crate::error::ShellError;
use crate::screenshot::{
    portal_request_path, PORTAL_PATH, PORTAL_REQUEST_INTERFACE, PORTAL_SERVICE,
};
use crate::shortcuts::PORTAL_SESSION_INTERFACE;

/// Freedesktop ScreenCast portal interface.
pub const PORTAL_SCREENCAST_INTERFACE: &str = "org.freedesktop.portal.ScreenCast";
/// Monitor source bit (`org.freedesktop.portal.ScreenCast.SelectSources`).
pub const SOURCE_TYPE_MONITOR: u32 = 1;
/// Window source bit (`org.freedesktop.portal.ScreenCast.SelectSources`).
pub const SOURCE_TYPE_WINDOW: u32 = 2;
/// Allow choosing either a monitor or an application window.
pub const SOURCE_TYPE_BOTH: u32 = SOURCE_TYPE_MONITOR | SOURCE_TYPE_WINDOW;
/// Embed cursor directly into the video frames.
pub const CURSOR_MODE_EMBEDDED: u32 = 2;

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Selected PipeWire video stream returned by `ScreenCast.Start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenCastStream {
    /// PipeWire node ID to pass to `pipewiresrc path=<node_id>`.
    pub node_id: u32,
    /// Optional stream width reported by the compositor.
    pub width: Option<u32>,
    /// Optional stream height reported by the compositor.
    pub height: Option<u32>,
    /// Optional source type (`1` = monitor, `2` = window).
    pub source_type: Option<u32>,
}

/// Active XDG Desktop Portal ScreenCast session.
///
/// Keeping this value alive keeps the portal session and PipeWire stream open.
/// Dropping it closes the portal session cleanly so the desktop's sharing
/// indicator disappears immediately.
pub struct ScreenCastSession {
    connection: zbus::blocking::Connection,
    session_handle: String,
    pipewire_fd: OwnedFd,
    /// Primary selected PipeWire stream.
    pub stream: ScreenCastStream,
}

impl ScreenCastSession {
    /// Borrowed PipeWire remote file descriptor from `OpenPipeWireRemote`.
    #[must_use]
    pub fn pipewire_fd(&self) -> BorrowedFd<'_> {
        self.pipewire_fd.as_fd()
    }

    /// D-Bus object path of the open portal session.
    #[must_use]
    pub fn session_handle(&self) -> &str {
        &self.session_handle
    }

    /// Closes the portal session explicitly.
    pub fn close(&self) {
        let _ = self.connection.call_method(
            Some(PORTAL_SERVICE),
            self.session_handle.as_str(),
            Some(PORTAL_SESSION_INTERFACE),
            "Close",
            &(),
        );
    }
}

impl Drop for ScreenCastSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Returns `true` when running inside a Wayland session.
#[must_use]
pub fn is_wayland_session() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    std::env::var("XDG_SESSION_TYPE")
        .map(|v| v.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false)
}

/// Checks whether `org.freedesktop.portal.ScreenCast` is exposed on the session bus.
#[must_use]
pub fn portal_screencast_available() -> bool {
    if cfg!(test) {
        return false;
    }
    let Ok(connection) = session() else {
        return false;
    };
    let Ok(proxy) = zbus::blocking::Proxy::new(
        &connection,
        PORTAL_SERVICE,
        PORTAL_PATH,
        PORTAL_SCREENCAST_INTERFACE,
    ) else {
        return false;
    };
    proxy.get_property::<u32>("version").is_ok()
}

/// Opens an interactive XDG ScreenCast portal picker (monitors and/or windows)
/// and returns an open PipeWire remote FD + stream node ID.
///
/// # Errors
///
/// Returns [`ShellError`] when the portal is unavailable, the user cancels the
/// picker, or `OpenPipeWireRemote` fails.
pub fn start_screencast(source_types: u32) -> Result<ScreenCastSession, ShellError> {
    let connection = session()?;
    let unique = connection
        .unique_name()
        .ok_or_else(|| ShellError::Bus("session bus has no unique name".into()))?
        .as_str()
        .to_owned();

    // 1. CreateSession
    let create_token = next_token("sc_create");
    let session_token = next_token("sc_sess");
    let mut create_opts: HashMap<&str, Value<'_>> = HashMap::new();
    create_opts.insert("handle_token", Value::from(create_token.as_str()));
    create_opts.insert("session_handle_token", Value::from(session_token.as_str()));

    let create_res = portal_call(&connection, &unique, &create_token, || {
        connection.call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(PORTAL_SCREENCAST_INTERFACE),
            "CreateSession",
            &(create_opts,),
        )
    })?;
    let session_handle = extract_session_handle(&create_res)
        .ok_or_else(|| ShellError::Bus("ScreenCast.CreateSession missing session_handle".into()))?;
    let session_path = ObjectPath::try_from(session_handle.as_str())
        .map_err(|e| ShellError::Bus(format!("invalid session_handle: {e}")))?;

    // 2. SelectSources
    let select_token = next_token("sc_select");
    let mut select_opts: HashMap<&str, Value<'_>> = HashMap::new();
    select_opts.insert("handle_token", Value::from(select_token.as_str()));
    select_opts.insert("types", Value::from(source_types.max(SOURCE_TYPE_MONITOR)));
    select_opts.insert("multiple", Value::from(false));
    select_opts.insert("cursor_mode", Value::from(CURSOR_MODE_EMBEDDED));

    if let Err(err) = portal_call(&connection, &unique, &select_token, || {
        connection.call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(PORTAL_SCREENCAST_INTERFACE),
            "SelectSources",
            &(session_path.clone(), select_opts),
        )
    }) {
        close_session_handle(&connection, &session_handle);
        return Err(err);
    }

    // 3. Start (shows native compositor screen/window picker)
    let start_token = next_token("sc_start");
    let mut start_opts: HashMap<&str, Value<'_>> = HashMap::new();
    start_opts.insert("handle_token", Value::from(start_token.as_str()));

    let start_res = match portal_call(&connection, &unique, &start_token, || {
        connection.call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(PORTAL_SCREENCAST_INTERFACE),
            "Start",
            &(session_path.clone(), "", start_opts),
        )
    }) {
        Ok(res) => res,
        Err(err) => {
            close_session_handle(&connection, &session_handle);
            return Err(err);
        }
    };

    let Some(stream) = parse_first_stream(&start_res) else {
        close_session_handle(&connection, &session_handle);
        return Err(ShellError::Bus(
            "ScreenCast.Start không trả về luồng PipeWire nào".into(),
        ));
    };

    // 4. OpenPipeWireRemote -> OwnedFd
    let open_opts: HashMap<&str, Value<'_>> = HashMap::new();
    let reply = match connection.call_method(
        Some(PORTAL_SERVICE),
        PORTAL_PATH,
        Some(PORTAL_SCREENCAST_INTERFACE),
        "OpenPipeWireRemote",
        &(session_path, open_opts),
    ) {
        Ok(r) => r,
        Err(err) => {
            close_session_handle(&connection, &session_handle);
            return Err(ShellError::Bus(err.to_string()));
        }
    };
    let zfd: ZbusOwnedFd = match reply.body().deserialize() {
        Ok(fd) => fd,
        Err(err) => {
            close_session_handle(&connection, &session_handle);
            return Err(ShellError::Bus(err.to_string()));
        }
    };
    let pipewire_fd: OwnedFd = zfd.into();

    Ok(ScreenCastSession {
        connection,
        session_handle,
        pipewire_fd,
        stream,
    })
}

fn next_token(prefix: &str) -> String {
    let seq = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("zalo_{prefix}_{}_{seq}_{nanos}", std::process::id())
}

fn close_session_handle(connection: &zbus::blocking::Connection, handle: &str) {
    let _ = connection.call_method(
        Some(PORTAL_SERVICE),
        handle,
        Some(PORTAL_SESSION_INTERFACE),
        "Close",
        &(),
    );
}

fn portal_call<F>(
    connection: &zbus::blocking::Connection,
    unique: &str,
    token: &str,
    invoke: F,
) -> Result<HashMap<String, OwnedValue>, ShellError>
where
    F: FnOnce() -> zbus::Result<zbus::Message>,
{
    let expected_path = portal_request_path(unique, token);
    let request_proxy = zbus::blocking::Proxy::new(
        connection,
        PORTAL_SERVICE,
        expected_path.as_str(),
        PORTAL_REQUEST_INTERFACE,
    )
    .map_err(|error| ShellError::Bus(error.to_string()))?;
    let mut signals = request_proxy
        .receive_signal("Response")
        .map_err(|error| ShellError::Bus(error.to_string()))?;

    invoke().map_err(|error| ShellError::Bus(error.to_string()))?;

    let message = signals
        .next()
        .ok_or_else(|| ShellError::Bus("portal Response stream closed".into()))?;
    let (status, results): (u32, HashMap<String, OwnedValue>) = message
        .body()
        .deserialize()
        .map_err(|error| ShellError::Bus(error.to_string()))?;
    if status != 0 {
        return Err(ShellError::Bus(format!(
            "Người dùng đã hủy hoặc từ chối chia sẻ màn hình (status {status})"
        )));
    }
    Ok(results)
}

fn extract_session_handle(results: &HashMap<String, OwnedValue>) -> Option<String> {
    let val = results.get("session_handle")?;
    let v: &Value<'_> = val;
    match v {
        Value::Str(s) => Some(s.as_str().to_owned()),
        Value::ObjectPath(p) => Some(p.as_str().to_owned()),
        _ => None,
    }
}

/// Parses the first `(node_id, props)` tuple from `ScreenCast.Start` `results["streams"]`.
pub(crate) fn parse_first_stream(
    results: &HashMap<String, OwnedValue>,
) -> Option<ScreenCastStream> {
    let val = results.get("streams")?;
    let Value::Array(arr) = &**val else {
        return None;
    };
    let first = arr.first()?;
    let Value::Structure(s) = first else {
        return None;
    };
    let fields = s.fields();
    let node_id = match fields.first()? {
        Value::U32(id) => *id,
        _ => return None,
    };
    let mut width = None;
    let mut height = None;
    let mut source_type = None;
    if let Some(Value::Dict(dict)) = fields.get(1) {
        for (k, v) in dict.iter() {
            let Value::Str(key) = k else {
                continue;
            };
            let inner = match v {
                Value::Value(boxed) => boxed.as_ref(),
                other => other,
            };
            match key.as_str() {
                "size" => {
                    let (w, h) = parse_stream_size(inner);
                    width = w;
                    height = h;
                }
                "source_type" => {
                    if let Value::U32(st) = inner {
                        source_type = Some(*st);
                    }
                }
                _ => {}
            }
        }
    }
    Some(ScreenCastStream {
        node_id,
        width,
        height,
        source_type,
    })
}

fn parse_stream_size(val: &Value<'_>) -> (Option<u32>, Option<u32>) {
    let inner = match val {
        Value::Value(boxed) => boxed.as_ref(),
        other => other,
    };
    let Value::Structure(s) = inner else {
        return (None, None);
    };
    let fields = s.fields();
    if fields.len() < 2 {
        return (None, None);
    }
    let to_u32 = |v: &Value<'_>| -> Option<u32> {
        match v {
            Value::I32(n) if *n > 0 => Some(*n as u32),
            Value::U32(n) if *n > 0 => Some(*n),
            _ => None,
        }
    };
    match (to_u32(&fields[0]), to_u32(&fields[1])) {
        (Some(w), Some(h)) => (Some(w), Some(h)),
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_session_handle_from_string_or_object_path() {
        let mut map = HashMap::new();
        map.insert(
            "session_handle".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from(
                "/org/freedesktop/portal/desktop/session/1_42/zalo_sc",
            )),
        );
        assert_eq!(
            extract_session_handle(&map).as_deref(),
            Some("/org/freedesktop/portal/desktop/session/1_42/zalo_sc")
        );
    }

    #[test]
    fn parses_first_stream_node_and_dimensions() {
        let mut props: HashMap<String, OwnedValue> = HashMap::new();
        let size_val: OwnedValue = Value::from((1920_i32, 1080_i32)).try_into().unwrap();
        props.insert("size".to_owned(), size_val);
        props.insert("source_type".to_owned(), OwnedValue::from(1_u32));

        let streams_val: OwnedValue = Value::from(vec![(77_u32, props)]).try_into().unwrap();
        let mut results = HashMap::new();
        results.insert("streams".to_owned(), streams_val);

        let stream = parse_first_stream(&results).expect("stream");
        assert_eq!(stream.node_id, 77);
        assert_eq!(stream.width, Some(1920));
        assert_eq!(stream.height, Some(1080));
        assert_eq!(stream.source_type, Some(1));
    }
}
