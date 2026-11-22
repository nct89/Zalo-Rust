//! Single-instance Windows Named Pipe IPC (`\\.\pipe\zalo-rust-<app_id>`).
//!
//! A second launch forwards `--show`, `--toggle-mute`, or `--quit` without
//! touching TCP/IP sockets (so Windows Firewall never prompts).

use std::path::PathBuf;

#[cfg(any(target_os = "windows", test))]
use crate::tray::TrayEvent;

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
    #[cfg(any(target_os = "windows", test))]
    const fn wire(self) -> &'static [u8] {
        match self {
            Self::Show => b"SHOW\n",
            Self::ToggleMute => b"MUTE\n",
            Self::Quit => b"QUIT\n",
        }
    }

    #[cfg(any(target_os = "windows", test))]
    fn parse(line: &str) -> Option<Self> {
        match line.trim() {
            "SHOW" => Some(Self::Show),
            "MUTE" => Some(Self::ToggleMute),
            "QUIT" => Some(Self::Quit),
            _ => None,
        }
    }

    #[cfg(any(target_os = "windows", test))]
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

/// Named pipe path: `\\.\pipe\zalo-rust-<sanitized_app_id>`.
#[must_use]
pub fn instance_socket_path(app_id: &str) -> PathBuf {
    let clean: String = app_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    PathBuf::from(format!(r"\\.\pipe\zalo-rust-{clean}"))
}

/// `true` when an instance owning `app_id` replies `PONG`.
#[must_use]
pub fn ping_instance(app_id: &str) -> bool {
    #[cfg(target_os = "windows")]
    if !cfg!(test) {
        return win_pipe::send(&instance_socket_path(app_id), b"PING\n", "PONG");
    }
    let _ = app_id;
    false
}

/// Forward `command` to a running instance. `true` when it acknowledged.
#[must_use]
pub fn try_send_instance_command(app_id: &str, command: InstanceCommand) -> bool {
    #[cfg(target_os = "windows")]
    if !cfg!(test) {
        return win_pipe::send(&instance_socket_path(app_id), command.wire(), "OK");
    }
    let _ = (app_id, command);
    false
}

/// Start a background Named Pipe listener for `app_id`.
pub fn start_instance_listener(app_id: &str) {
    #[cfg(target_os = "windows")]
    if !cfg!(test) {
        win_pipe::start(instance_socket_path(app_id));
    }
    #[cfg(not(target_os = "windows"))]
    let _ = app_id;
}

/// Stop the listener (no-op; Windows reclaims the named pipe on process exit).
pub fn stop_instance_listener(_app_id: &str) {}

#[cfg(target_os = "windows")]
mod win_pipe {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW,
        PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    use super::InstanceCommand;
    use crate::tray::record_tray_event;

    fn wide(path: &Path) -> Vec<u16> {
        OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn send(path: &Path, payload: &[u8], expected_prefix: &str) -> bool {
        let name = wide(path);
        let _ = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 250) };
        let handle = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        let Ok(handle) = handle else {
            return false;
        };
        if handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let ok = write_and_read(handle, payload, expected_prefix);
        let _ = unsafe { CloseHandle(handle) };
        ok
    }

    fn write_and_read(handle: HANDLE, payload: &[u8], expected_prefix: &str) -> bool {
        let mut written = 0_u32;
        if unsafe { WriteFile(handle, Some(payload), Some(&mut written), None) }.is_err() {
            return false;
        }
        let _ = unsafe { FlushFileBuffers(handle) };
        let mut buf = [0_u8; 64];
        let mut read = 0_u32;
        if unsafe { ReadFile(handle, Some(&mut buf), Some(&mut read), None) }.is_err() {
            return false;
        }
        let text = String::from_utf8_lossy(&buf[..read as usize]);
        text.trim_start().starts_with(expected_prefix)
    }

    pub(super) fn start(path: PathBuf) {
        let _ = std::thread::Builder::new()
            .name("zalo-instance".into())
            .spawn(move || serve(path));
    }

    fn serve(path: PathBuf) {
        let name = wide(&path);
        loop {
            let handle = unsafe {
                CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    256,
                    256,
                    0,
                    None,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                break;
            }
            let connected = match unsafe { ConnectNamedPipe(handle, None) } {
                Ok(()) => true,
                Err(err) => err.code() == ERROR_PIPE_CONNECTED.to_hresult(),
            };
            if connected {
                handle_client(handle);
            }
            let _ = unsafe { DisconnectNamedPipe(handle) };
            let _ = unsafe { CloseHandle(handle) };
        }
    }

    fn handle_client(handle: HANDLE) {
        let mut buf = [0_u8; 128];
        let mut read = 0_u32;
        if unsafe { ReadFile(handle, Some(&mut buf), Some(&mut read), None) }.is_err() || read == 0 {
            return;
        }
        let line = String::from_utf8_lossy(&buf[..read as usize]);
        let reply: &[u8] = if line.trim() == "PING" {
            b"PONG\n"
        } else if let Some(cmd) = InstanceCommand::parse(&line) {
            record_tray_event(cmd.into_tray_event());
            b"OK\n"
        } else {
            b"ERR\n"
        };
        let mut written = 0_u32;
        let _ = unsafe { WriteFile(handle, Some(reply), Some(&mut written), None) };
        let _ = unsafe { FlushFileBuffers(handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_instance_and_profile_args() {
        let args = vec![
            "zalo-gpui.exe".into(),
            "--profile=work".into(),
            "--toggle-mute".into(),
        ];
        assert_eq!(
            instance_command_from_args(&args),
            Some(InstanceCommand::ToggleMute)
        );
        assert_eq!(profile_arg_from_args(&args), Some("work".into()));
        assert_eq!(
            instance_socket_path("zalo-gpui"),
            PathBuf::from(r"\\.\pipe\zalo-rust-zalo-gpui")
        );
        assert_eq!(InstanceCommand::parse("SHOW\n"), Some(InstanceCommand::Show));
        assert_eq!(InstanceCommand::Show.wire(), b"SHOW\n");
        assert_eq!(InstanceCommand::Quit.into_tray_event(), TrayEvent::Quit);
    }
}
