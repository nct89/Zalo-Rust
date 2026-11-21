//! CLI: resume a saved session and print account / friend / thread info.

use anyhow::{Context, Result};
use std::fs;
use zalo_client::{Device, StoredSession, ZaloApi};
use zalo_core::AppPaths;

#[tokio::main]
async fn main() -> Result<()> {
    let paths = AppPaths::from_env("zalo-rust").context("resolve application directories")?;
    let config_dir = paths.config_dir().to_path_buf();
    fs::create_dir_all(&config_dir).context("create config directory")?;
    zalo_protocol::init_from_dir(&config_dir);

    let device = Device::load_or_create(&config_dir).context("load device identity")?;
    let mut session =
        StoredSession::load(&config_dir).context("load session (run zalo-login first)")?;
    if session.zpw_sek().is_none() {
        anyhow::bail!("no zpw_sek in session; run zalo-login and scan the QR first");
    }

    let mut api = ZaloApi::from_session(device, &session).context("resume session")?;
    println!("session resumed");

    let info = api.get_login_info().await.context("getLoginInfo")?;
    session.apply_login_info(
        info.uin.clone(),
        info.zpw_enk.clone(),
        info.phone_number.clone(),
        info.display_name.clone(),
    );
    if info.send2me_id.is_some() {
        session.send2me_id = info.send2me_id.clone();
    }
    println!("uin: {}", mask(&info.uin));
    println!(
        "send2me_id: {}",
        info.send2me_id.as_deref().unwrap_or("(none)")
    );
    println!(
        "phone: {}",
        info.phone_number
            .as_deref()
            .map(mask_phone)
            .unwrap_or_else(|| "(none)".to_owned())
    );
    println!("name: {}", info.display_name.as_deref().unwrap_or("(none)"));

    let friends = api.get_friends().await.context("getfriends")?;
    if let Ok(me) = api.get_me().await {
        if let Some(name) = me
            .get("data")
            .and_then(|d| d.get("displayName"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            session.display_name = Some(name.to_owned());
            println!("me display_name: {name}");
        }
    }
    if session.display_name.is_none() {
        if let Some(self_id) = session.send2me_id.as_deref() {
            session.display_name = friend_name_for_id(&friends, self_id);
        }
    }
    let names = friend_names(&friends);
    println!("friends: {}", names.len());
    for name in names.iter().take(10) {
        println!("  - {name}");
    }

    let messages = api.get_last_messages().await.context("get-last-msgs")?;
    let (direct, groups) = thread_counts(&messages);
    println!("known conversations: {direct} direct + {groups} groups");

    let path = session.save(&config_dir).context("save session")?;
    println!("session updated at {}", path.display());
    Ok(())
}

fn friend_name_for_id(value: &serde_json::Value, target_id: &str) -> Option<String> {
    value
        .get("data")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .find(|friend| {
            friend
                .get("userId")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| id == target_id)
        })
        .and_then(|friend| {
            friend
                .get("displayName")
                .or_else(|| friend.get("zaloName"))
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        })
}

fn friend_names(value: &serde_json::Value) -> Vec<String> {
    value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .map(|friends| {
            friends
                .iter()
                .filter_map(|friend| {
                    friend
                        .get("displayName")
                        .or_else(|| friend.get("zaloName"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn thread_counts(value: &serde_json::Value) -> (usize, usize) {
    let data = value.get("data");
    let direct = data
        .and_then(|data| data.get("msgs"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let groups = data
        .and_then(|data| data.get("groupMsgs"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    (direct, groups)
}

fn mask(value: &str) -> String {
    if value.len() <= 8 {
        return "(hidden)".to_owned();
    }
    format!("{}…", &value[..8])
}

fn mask_phone(value: &str) -> String {
    if value.len() < 4 {
        return "(hidden)".to_owned();
    }
    format!("…{}", &value[value.len() - 4..])
}
