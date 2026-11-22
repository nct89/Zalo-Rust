//! Single-instance UNIX socket. A second launch forwards `--show`, `--toggle-mute`, or `--quit`.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::tray::{record_tray_event, TrayEvent};

/// Commands accepted by a running Zalo instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceCommand {
    /// Bring the main window forward.
    Show,
    /// Toggle message notifications.
    ToggleMute,
    /// Quit the running client.
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

/// Extract an [`InstanceCommand`] from CLI arguments.
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

/// Extract `--profile <name>` or `--profile=<name>`.
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

/// Socket location: `$TMPDIR/zalo-macos-<app_id>.sock`, else `~/Library/Caches`.
#[must_use]
pub fn instance_socket_path(app_id: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join("Library/Caches/zalo-rust"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("zalo-macos-{app_id}.sock"))
}

/// `true` when an instance owning `app_id` replies `PONG`.
#[must_use]
pub fn ping_instance(app_id: &str) -> bool {
    send_to_socket(&instance_socket_path(app_id), b"PING\n", "PONG")
}

/// Forward `command` to a running instance. `true` when it acknowledged.
#[must_use]
pub fn try_send_instance_command(app_id: &str, command: InstanceCommand) -> bool {
    send_to_socket(&instance_socket_path(app_id), command.wire(), "OK")
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

/// Start a background listener for `app_id`.
pub fn start_instance_listener(app_id: &str) {
    start_instance_listener_at(instance_socket_path(app_id));
}

pub(crate) fn start_instance_listener_at(path: PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(err) if err.kind() == ErrorKind::AddrInUse => {
            if send_to_socket(&path, b"PING\n", "PONG") {
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

/// Remove the control socket when no live peer answers.
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
        assert!(instance_command_from_args(&["zalo-gpui".into(), "--hidden".into()]).is_none());
        assert_eq!(
            profile_arg_from_args(&["zalo".into(), "--profile".into(), "work".into()]),
            Some("work".into())
        );
    }

    #[test]
    fn socket_roundtrip_delivers_commands_and_recovers_stale_file() {
        let _guard = crate::tray::TEST_EVENT_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let sock = std::env::temp_dir().join(format!(
            "zalo-mac-inst-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&sock, "stale").expect("stale");
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
            vec![
                TrayEvent::Activate,
                TrayEvent::ToggleMute,
                TrayEvent::Quit
            ]
        );
        let _ = std::fs::remove_file(&sock);
    }
}
