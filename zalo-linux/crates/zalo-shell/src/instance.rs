//! Pure-Rust single-instance UNIX socket IPC for Dock quicklists & CLI flags.
//!
//! When Zalo is hidden in the tray or running on a tray-less desktop (e.g.
//! stock GNOME Wayland), clicking the launcher icon or a `.desktop` Quicklist
//! action (`--show`, `--toggle-mute`, `--quit`) forwards the command to the live
//! process and exits immediately instead of spawning a second client.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::tray::{record_tray_event, TrayEvent};

/// Commands accepted by a running Zalo instance over its control socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceCommand {
    /// Bring the main window forward (or reopen it from the tray).
    Show,
    /// Toggle desktop message notifications (`notifyOff`).
    ToggleMute,
    /// Quit the running client cleanly.
    Quit,
}

impl InstanceCommand {
    const fn wire(self) -> &'static [u8] {
        match self {
            Self::Show => b"SHOW\n",
            Self::ToggleMute => b"MUTE\n",
            Self::Quit => b"QUIT\n",
        }
    }

    fn parse(line: &str) -> Option<Self> {
        match line.trim() {
            "SHOW" => Some(Self::Show),
            "MUTE" => Some(Self::ToggleMute),
            "QUIT" => Some(Self::Quit),
            _ => None,
        }
    }

    const fn into_tray_event(self) -> TrayEvent {
        match self {
            Self::Show => TrayEvent::Activate,
            Self::ToggleMute => TrayEvent::ToggleMute,
            Self::Quit => TrayEvent::Quit,
        }
    }
}

/// Extract an [`InstanceCommand`] from CLI arguments (`--show`, `--toggle-mute`, `--quit`).
#[must_use]
pub fn instance_command_from_args(args: &[String]) -> Option<InstanceCommand> {
    if args.iter().any(|arg| arg == "--quit") {
        Some(InstanceCommand::Quit)
    } else if args.iter().any(|arg| arg == "--toggle-mute") {
        Some(InstanceCommand::ToggleMute)
    } else if args.iter().any(|arg| arg == "--show") {
        Some(InstanceCommand::Show)
    } else {
        None
    }
}

/// Extract a `--profile <name>` or `--profile=<name>` CLI flag.
#[must_use]
pub fn profile_arg_from_args(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if let Some(rest) = arg.strip_prefix("--profile=") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        } else if arg == "--profile" {
            if let Some(next) = iter.next() {
                let trimmed = next.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
        }
    }
    None
}

/// Socket location for `app_id`: `$XDG_RUNTIME_DIR/zalo-linux-<app_id>.sock`.
#[must_use]
pub fn instance_socket_path(app_id: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("zalo-linux-{app_id}.sock"))
}

/// Return `true` when an instance owning `app_id` replies `PONG` to `PING`.
#[must_use]
pub fn ping_instance(app_id: &str) -> bool {
    let path = instance_socket_path(app_id);
    send_to_socket(&path, b"PING\n", "PONG")
}

/// Attempt to forward `command` to an already running instance of `app_id`.
///
/// Returns `true` if an existing instance acknowledged the command.
#[must_use]
pub fn try_send_instance_command(app_id: &str, command: InstanceCommand) -> bool {
    let path = instance_socket_path(app_id);
    send_to_socket(&path, command.wire(), "OK")
}

fn send_to_socket(path: &Path, payload: &[u8], expected_prefix: &str) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    let timeout = Some(Duration::from_millis(400));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    if stream.write_all(payload).is_err() || stream.flush().is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut ack = String::new();
    reader.read_line(&mut ack).is_ok() && ack.trim_start().starts_with(expected_prefix)
}

/// Start a background listener for single-instance commands targeting `app_id`.
pub fn start_instance_listener(app_id: &str) {
    let path = instance_socket_path(app_id);
    start_instance_listener_at(path);
}

fn start_instance_listener_at(path: PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(err) if err.kind() == ErrorKind::AddrInUse => {
            if send_to_socket(&path, b"PING\n", "PONG") {
                // Another live instance owns the socket.
                return;
            }
            let _ = std::fs::remove_file(&path);
            let Ok(listener) = UnixListener::bind(&path) else {
                return;
            };
            listener
        }
        Err(_) => return,
    };
    let _ = std::thread::Builder::new()
        .name("zalo-instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                handle_client(stream);
            }
        });
}

fn handle_client(mut stream: UnixStream) {
    let timeout = Some(Duration::from_millis(400));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let trimmed = line.trim();
    if trimmed == "PING" {
        let _ = stream.write_all(b"PONG\n");
        let _ = stream.flush();
        return;
    }
    if let Some(cmd) = InstanceCommand::parse(trimmed) {
        record_tray_event(cmd.into_tray_event());
        let _ = stream.write_all(b"OK\n");
        let _ = stream.flush();
    }
}

/// Remove the control socket for `app_id` on clean shutdown.
pub fn stop_instance_listener(app_id: &str) {
    let path = instance_socket_path(app_id);
    if !send_to_socket(&path, b"PING\n", "PONG") {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::take_tray_events;

    #[test]
    fn parses_cli_flags_deterministically() {
        assert_eq!(
            instance_command_from_args(&["zalo-gpui".into(), "--show".into()]),
            Some(InstanceCommand::Show)
        );
        assert_eq!(
            instance_command_from_args(&["zalo-gpui".into(), "--toggle-mute".into()]),
            Some(InstanceCommand::ToggleMute)
        );
        assert_eq!(
            instance_command_from_args(&["zalo-gpui".into(), "--quit".into()]),
            Some(InstanceCommand::Quit)
        );
        assert_eq!(
            instance_command_from_args(&["zalo-gpui".into(), "--hidden".into()]),
            None
        );
    }

    #[test]
    fn socket_roundtrip_delivers_commands_and_recovers_stale_file() {
        let _guard = crate::tray::TEST_EVENT_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let sock = std::env::temp_dir().join(format!(
            "zalo-inst-test-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&sock, "stale").expect("write stale file");
        let _ = take_tray_events();
        start_instance_listener_at(sock.clone());
        std::thread::sleep(Duration::from_millis(40));
        assert!(send_to_socket(&sock, b"PING\n", "PONG"));
        assert!(send_to_socket(&sock, InstanceCommand::Show.wire(), "OK"));
        assert!(send_to_socket(
            &sock,
            InstanceCommand::ToggleMute.wire(),
            "OK"
        ));
        assert!(send_to_socket(&sock, InstanceCommand::Quit.wire(), "OK"));
        assert_eq!(
            take_tray_events(),
            vec![TrayEvent::Activate, TrayEvent::ToggleMute, TrayEvent::Quit]
        );
        let _ = std::fs::remove_file(&sock);
    }
}
