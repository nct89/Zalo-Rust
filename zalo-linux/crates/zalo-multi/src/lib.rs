//! Pure-Rust multi-account core compatible with ZaloMulti-Linux 3.0.5 (`lza.lol`).
//!
//! Shares `~/.config/ZaloMulti/`, `~/.local/share/ZaloMulti/Profiles/`, the
//! `zmh1-linux` hardware fingerprint, and the Ed25519-signed `zms2` / `zmr1`
//! session protocol so existing ZaloMulti logins and clones work seamlessly
//! inside native `zalo-gpui`.

#![forbid(unsafe_code)]

pub mod account;
pub mod hwid;
pub mod launcher;
pub mod paths;
pub mod profiles;
pub mod storage;

pub use account::{
    AccountError, AccountStatus, SessionClaims, BUY_MORE_URL, NOT_LOGGED_IN, PLATFORM,
    REMEMBER_FOR_SECS, SESSION_PUBLIC_KEY,
};
pub use hwid::{cached_hwid, format_hwid, material_of, merge_parts, HwidParts};
pub use launcher::{
    config_home, data_home, is_running, launch_clone, profile_app_id, read_proxy, save_proxy,
    stop_all, stop_clone, LaunchOutcome,
};
pub use paths::{
    app_data_dir, default_profile_root, profile_root, user_config_home, user_data_home,
};
pub use profiles::{
    account_clone_root, add_profile, canonical_profile_name, contained_join, decide_add,
    decide_launch, delete_profile_dir, list_profile_names, profile_dir, read_clone_zalo_name,
    read_phone, rename_profile, save_profile_phone,
};
pub use storage::{ensure_writable, set_profile_root, validate_root, RootMoveOutcome};
