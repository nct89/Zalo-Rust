//! Binary `zalo-gpui`: native Zalo desktop client built with GPUI (Rust).
//! Automatically opens the main UI when `ZALO_GPUI_PREVIEW=1` or a saved session exists.
//!
//! `--hidden` / `--start-hidden` (autostart entry): stay in the tray.

use gpui::{App, Application};

fn main() {
    if let Ok(xdg) = zalo_core::XdgDirs::from_env() {
        zalo_protocol::init_from_dir(&xdg.config_home().join("zalo-rust"));
    } else {
        let _ = zalo_protocol::apply_env_overrides();
    }
    let args: Vec<String> = std::env::args().collect();
    let installer_mode = args.iter().any(|a| a == "--install" || a == "--setup");
    if installer_mode {
        Application::new().run(move |cx: &mut App| {
            zalo_gpui::install_fonts(cx);
            zalo_gpui::bind_line_keys(cx);
            zalo_gpui::open_installer_window(cx);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
        return;
    }

    if let Some(cmd) = zalo_shell::instance_command_from_args(&args) {
        if zalo_shell::try_send_instance_command(zalo_shell::GPUI_APP_ID, cmd) {
            return;
        }
        if cmd == zalo_shell::InstanceCommand::Quit {
            return;
        }
    } else if !args
        .iter()
        .any(|a| a == "--login" || a == "--allow-multiple")
        && std::env::var_os("ZALO_MULTI_INSTANCE").is_none()
        && std::env::var_os("ZALO_GPUI_PREVIEW").is_none()
        && std::env::var_os("ZALO_GPUI_NO_TRAY").is_none()
        && zalo_shell::try_send_instance_command(
            zalo_shell::GPUI_APP_ID,
            zalo_shell::InstanceCommand::Show,
        )
    {
        return;
    }

    Application::new().run(move |cx: &mut App| {
        zalo_gpui::install_fonts(cx);
        zalo_gpui::bind_line_keys(cx);
        zalo_gpui::start_tray(cx);
        zalo_gpui::restore_saved_mic_volume();
        let force_login = args.iter().any(|a| a == "--login");
        let hidden = args
            .iter()
            .any(|a| a == "--hidden" || a == "--start-hidden");
        if hidden && !force_login && zalo_gpui::start_hidden(cx) {
            // Tray only; a click opens the window.
        } else if !force_login && zalo_gpui::preview_shell_requested() {
            zalo_gpui::open_shell_window(cx);
        } else {
            zalo_gpui::open_login_window(cx);
        }
        cx.on_window_closed(zalo_gpui::on_last_window_closed)
            .detach();
        cx.activate(true);
    });
}
