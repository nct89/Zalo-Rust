//! CLI: open the realtime `zpw_ws` socket with the saved session and print
//! what arrives. Stops after `ZALO_LISTEN_SEC` seconds (default 60).
//!
//! Only one realtime connection per account is kept by Zalo; if another client
//! takes it the listener prints `replaced` and exits.

use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;
use zalo_client::{run_listener, Device, ListenerEvent, StoredSession, ZaloApi};
use zalo_core::XdgDirs;

#[tokio::main]
async fn main() -> Result<()> {
    let xdg = XdgDirs::from_env().context("resolve XDG directories")?;
    let config_dir = xdg.config_home().join("zalo-rust");
    let device = Device::load_or_create(&config_dir).context("load device identity")?;
    let session =
        StoredSession::load(&config_dir).context("load session (run zalo-login first)")?;
    if session.zpw_sek().is_none() {
        anyhow::bail!("no zpw_sek in session; run zalo-login and scan the QR first");
    }
    let mut api = ZaloApi::from_session(device, &session).context("resume session")?;
    api.get_login_info().await.context("getLoginInfo")?;
    let server_info = api.get_server_info().await.ok();
    let mut config = api.listener_config(server_info.as_ref());
    config.since_user = std::env::var("ZALO_LISTEN_SINCE_USER").ok();
    config.since_group = std::env::var("ZALO_LISTEN_SINCE_GROUP").ok();
    println!(
        "endpoints: {:?}, ping every {}s",
        config.endpoints,
        config.ping_interval.as_secs()
    );

    let seconds: u64 = std::env::var("ZALO_LISTEN_SEC")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let listen = run_listener(config, |event| {
        match event {
            ListenerEvent::Connected => println!("connected"),
            ListenerEvent::Ready => println!("ready (cipher key received)"),
            ListenerEvent::Messages { group, items, .. } => {
                for item in items {
                    if std::env::var_os("ZALO_LISTEN_RAW").is_some() {
                        println!("{item}");
                    }
                    println!(
                        "{} {} msg from {} to {}: {}",
                        if group { "group" } else { "user" },
                        field(&item, "msgType"),
                        mask(&field(&item, "uidFrom")),
                        mask(&field(&item, "idTo")),
                        preview(&item),
                    );
                }
            }
            ListenerEvent::Event { cmd, sub_cmd, .. } => println!("event cmd {cmd}/{sub_cmd}"),
            ListenerEvent::Status(status) => println!(
                "status: {} read elsewhere, {} seen",
                status.read.len(),
                status.seen.len()
            ),
            ListenerEvent::Typing(list) => println!("typing: {} người", list.len()),
            ListenerEvent::Disconnected(why) => println!("disconnected: {why}"),
            ListenerEvent::Replaced => println!("replaced by another client"),
        }
        true
    });
    match tokio::time::timeout(Duration::from_secs(seconds), listen).await {
        Ok(result) => result.context("listener")?,
        Err(_) => println!("stopped after {seconds}s"),
    }
    Ok(())
}

fn field(item: &Value, key: &str) -> String {
    match item.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn preview(item: &Value) -> String {
    let text = match item.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(_) => format!("[{}]", field(item, "msgType")),
        None => String::new(),
    };
    let short: String = text.chars().take(40).collect();
    if short.len() < text.len() {
        format!("{short}…")
    } else {
        short
    }
}

fn mask(value: &str) -> String {
    let n = value.chars().count();
    if n <= 4 {
        return value.to_owned();
    }
    let tail: String = value.chars().skip(n - 4).collect();
    format!("…{tail}")
}
