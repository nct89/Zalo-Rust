//! CLI: request a QR code, wait for the phone and persist the session.

use anyhow::{Context, Result};
use std::env;
use std::fs;
use zalo_client::{render_terminal, Device, StoredSession, ZaloApi};
use zalo_core::AppPaths;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let qr_only = args.iter().any(|arg| arg == "--qr-only");
    let debug = args.iter().any(|arg| arg == "--debug");

    let paths = AppPaths::from_env("zalo-rust").context("resolve application directories")?;
    let config_dir = paths.config_dir().to_path_buf();
    fs::create_dir_all(&config_dir).context("create config directory")?;
    zalo_protocol::init_from_dir(&config_dir);

    let device = Device::load_or_create(&config_dir).context("load device identity")?;
    println!("device: {device}");
    let api = ZaloApi::new(device).context("build HTTP client")?;

    println!("getServerInfo...");
    let server_info = api.get_server_info().await.context("getServerInfo")?;
    println!("getServerInfo: ok");
    if debug {
        println!("{}", serde_json::to_string_pretty(&server_info)?);
    }

    println!("requesting QR...");
    let ticket = api.request_qr().await.context("request QR")?;
    println!("token: {}", ticket.token_id);
    println!("QR URL: {}", ticket.qr_url);
    println!("{}", render_terminal(&ticket.qr_url)?);
    println!(
        "Scan this code with Zalo on your phone (valid {}s).",
        ticket.timeout_seconds
    );

    if qr_only {
        println!("--qr-only: not polling, exiting.");
        return Ok(());
    }

    println!("waiting for scan...");
    let scan = api.wait_for_scan(&ticket).await.context("wait for scan")?;
    if debug {
        println!("scan response: {}", serde_json::to_string_pretty(&scan)?);
    }
    println!("scanned; confirming...");
    let confirm = api
        .wait_for_confirm(&ticket)
        .await
        .context("wait for confirm")?;
    if debug {
        println!(
            "confirm response: {}",
            serde_json::to_string_pretty(&confirm)?
        );
    }

    let cookies = api.cookie_headers();
    println!("cookies received: {}", cookies.len());
    let session = StoredSession::from_cookie_headers(cookies);
    let path = session.save(&config_dir).context("save session")?;
    println!("session saved to {}", path.display());
    println!("next phase: call getLoginInfo with the saved cookie jar");
    Ok(())
}
