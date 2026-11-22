//! ZaloMulti account authentication & Ed25519 `zms2` session verification
//! (`account.js` parity for `https://api.lza.lol/zalomulti`).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use rand::RngCore;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hwid::{cached_hwid, format_hwid};
use crate::paths::app_data_dir;

/// Server Ed25519 public key (`GET https://api.lza.lol/zalomulti/public-key`).
pub const SESSION_PUBLIC_KEY: &str = "RI5F9ffKfGK6Jg9OKtWeLS2YR_CyP22_yd-ary4rQ28";

/// Default ZaloMulti account API base URL.
pub const DEFAULT_API_BASE: &str = "https://api.lza.lol/zalomulti";

/// Purchase / upgrade page URL for Linux clone slots (`platform.js` parity).
pub const BUY_MORE_URL: &str = "https://lza.lol/#nang-cap-linux";

/// Platform identifier embedded in `zms2` tokens.
pub const PLATFORM: &str = "linux";

/// Standard error message when not logged in.
pub const NOT_LOGGED_IN: &str = "Vui lòng đăng nhập để sử dụng ZaloMulti.";

/// Duration of "Ghi nhớ đăng nhập": 15 days from password sign-in.
pub const REMEMBER_FOR_SECS: i64 = 15 * 24 * 60 * 60;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// RFC 8410 Ed25519 PKCS#8 v1 16-byte prefix before the 32-byte private seed.
const ED25519_PKCS8_V1_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// Account errors returned by ZaloMulti auth operations.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// Network or HTTP failure talking to `api.lza.lol`.
    #[error("{0}")]
    Server(String),
    /// Local validation, crypto, or I/O error.
    #[error("{0}")]
    Local(String),
}

/// Verified claims extracted from an Ed25519-signed `zms2` / `zms1` session token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionClaims {
    /// Account user ID.
    pub uid: i64,
    /// Bound Linux hardware fingerprint.
    pub hwid: String,
    /// Maximum allowed clone profiles on Linux.
    pub max: usize,
    /// Whether the account has a supporter / pro tier.
    pub donated: bool,
    /// Token expiration UNIX timestamp (seconds).
    pub exp: i64,
}

/// Snapshot of the current ZaloMulti account state for UI display.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountStatus {
    /// `true` when a valid, non-expired `zms2`/`zms1` session exists for this machine.
    pub logged_in: bool,
    /// Account email (may be pre-filled even if expired).
    pub email: String,
    /// Account numeric UID when logged in.
    pub uid: Option<i64>,
    /// Effective clone limit (`0` when logged out).
    pub max_profiles: usize,
    /// Supporter / pro flag.
    pub donated: bool,
    /// Session expiry timestamp (UNIX seconds).
    pub session_exp: i64,
    /// Whether a refresh token is available (persisted or in-memory).
    pub has_refresh: bool,
    /// Formatted display HWID (`XXXXXX-XXXXXX-XXXXXX-XXXXXX`).
    pub hwid_display: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct SecretBlob {
    kind: String,
    data: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountStateFile {
    #[serde(default)]
    email: String,
    #[serde(default)]
    session: String,
    #[serde(default)]
    remember_until: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh: Option<SecretBlob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device: Option<SecretBlob>,
}

/// Device proof payload sent on `/auth/session` refreshes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProof {
    /// UNIX timestamp in seconds.
    pub ts: i64,
    /// Base64url Ed25519 signature over `zmr1|{hwid}|{ts}|{sha256(refresh_token)}`.
    pub sig: String,
}

#[derive(Default)]
struct MemorySecrets {
    refresh: String,
    device_pem: String,
    settled: bool,
}

fn memory_secrets() -> &'static Mutex<MemorySecrets> {
    static MEM: OnceLock<Mutex<MemorySecrets>> = OnceLock::new();
    MEM.get_or_init(|| Mutex::new(MemorySecrets::default()))
}

/// Current UNIX timestamp in seconds.
#[must_use]
pub fn now_sec() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compute `rememberUntil` timestamp (`now + 15 days` when `remember`, else `0`).
#[must_use]
pub const fn remember_until_for(now: i64, remember: bool) -> i64 {
    if remember {
        now + REMEMBER_FOR_SECS
    } else {
        0
    }
}

/// Return `true` when a saved login is still within its 15-day window.
#[must_use]
pub const fn saved_login_active(has_refresh: bool, remember_until: i64, now: i64) -> bool {
    has_refresh && remember_until > now
}

fn is_b64url_str(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Verify an Ed25519-signed `zms2` / `zms1` session token against `hwid` and `now`.
#[must_use]
pub fn verify_session_with_key(
    token: &str,
    hwid: &str,
    now: i64,
    pubkey_b64url: &str,
) -> Option<SessionClaims> {
    let mut parts = token.split('.');
    let p0 = parts.next()?;
    let p1 = parts.next()?;
    if parts.next().is_some() || !is_b64url_str(p0) || !is_b64url_str(p1) {
        return None;
    }
    let payload = URL_SAFE_NO_PAD.decode(p0).ok()?;
    let signature = URL_SAFE_NO_PAD.decode(p1).ok()?;
    if signature.len() != 64 {
        return None;
    }
    let pubkey_bytes = URL_SAFE_NO_PAD.decode(pubkey_b64url).ok()?;
    let verifier = UnparsedPublicKey::new(&ED25519, &pubkey_bytes);
    verifier.verify(&payload, &signature).ok()?;

    let text = std::str::from_utf8(&payload).ok()?;
    let fields: Vec<&str> = text.split('|').collect();
    let shape_ok = (fields.len() == 6 && fields[0] == "zms1")
        || (fields.len() == 7 && fields[0] == "zms2" && fields[6] == PLATFORM);
    if !shape_ok {
        return None;
    }
    let uid: i64 = fields[1].parse().ok()?;
    let token_hwid = fields[2];
    let max: usize = fields[3].parse().ok()?;
    let donated = fields[4] == "1";
    let exp: i64 = fields[5].parse().ok()?;
    if !(1..=32).contains(&max) || token_hwid != hwid || now >= exp {
        return None;
    }
    Some(SessionClaims {
        uid,
        hwid: token_hwid.to_owned(),
        max,
        donated,
        exp,
    })
}

/// Verify a `zms2` / `zms1` session token using the production `lza.lol` public key.
#[must_use]
pub fn verify_session(token: &str, hwid: &str, now: i64) -> Option<SessionClaims> {
    verify_session_with_key(token, hwid, now, SESSION_PUBLIC_KEY)
}

/// Generate a fresh Ed25519 device keypair, returning `(pkcs8_pem, device_key_b64url)`.
///
/// # Errors
///
/// Returns [`AccountError::Local`] if keypair construction fails.
pub fn generate_device_keypair() -> Result<(String, String), AccountError> {
    let mut seed = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| AccountError::Local("Không tạo được khóa thiết bị Ed25519.".into()))?;
    let mut der = Vec::with_capacity(48);
    der.extend_from_slice(&ED25519_PKCS8_V1_PREFIX);
    der.extend_from_slice(&seed);
    let b64 = STANDARD.encode(&der);
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        if let Ok(line) = std::str::from_utf8(chunk) {
            pem.push_str(line);
            pem.push('\n');
        }
    }
    pem.push_str("-----END PRIVATE KEY-----\n");
    let pub_b64url = URL_SAFE_NO_PAD.encode(pair.public_key().as_ref());
    Ok((pem, pub_b64url))
}

fn parse_ed25519_pem(pem: &str) -> Option<Ed25519KeyPair> {
    let b64: String = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("-----"))
        .collect();
    let der = STANDARD.decode(&b64).ok()?;
    if der.len() == 48 && der[..16] == ED25519_PKCS8_V1_PREFIX {
        Ed25519KeyPair::from_seed_unchecked(&der[16..48]).ok()
    } else {
        Ed25519KeyPair::from_pkcs8_maybe_unchecked(&der).ok()
    }
}

/// Sign the device refresh proof `zmr1|{hwid}|{ts}|{sha256(refresh_token)}`.
///
/// # Errors
///
/// Returns [`AccountError::Local`] when `pem` is not a valid Ed25519 PKCS#8 key.
pub fn device_proof(
    pem: &str,
    hwid: &str,
    refresh_token: &str,
    ts: i64,
) -> Result<DeviceProof, AccountError> {
    let pair = parse_ed25519_pem(pem)
        .ok_or_else(|| AccountError::Local("Khóa thiết bị không hợp lệ.".into()))?;
    let hash = hex::encode(Sha256::digest(refresh_token.as_bytes()));
    let message = format!("zmr1|{hwid}|{ts}|{hash}");
    let sig = pair.sign(message.as_bytes());
    Ok(DeviceProof {
        ts,
        sig: URL_SAFE_NO_PAD.encode(sig.as_ref()),
    })
}

fn machine_key(hwid: &str) -> [u8; 32] {
    let digest = Sha256::digest(format!("zm-remember-v1\n{hwid}").as_bytes());
    let mut key = [0_u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// Encrypt `plaintext` using the machine-bound AES-256-GCM key (`account.js` compatible).
///
/// Format: `<iv_b64url>.<tag_b64url>.<ct_b64url>`.
///
/// # Errors
///
/// Returns [`AccountError::Local`] if encryption fails.
pub fn seal_machine(plaintext: &str, hwid: &str) -> Result<String, AccountError> {
    let key = machine_key(hwid);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| AccountError::Local("Không khởi tạo được khóa mã hóa.".into()))?;
    let mut iv = [0_u8; 12];
    rand::thread_rng().fill_bytes(&mut iv);
    let mut buf = plaintext.as_bytes().to_vec();
    let tag = cipher
        .encrypt_in_place_detached(Nonce::from_slice(&iv), b"", &mut buf)
        .map_err(|_| AccountError::Local("Không mã hóa được bí mật.".into()))?;
    Ok(format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(iv),
        URL_SAFE_NO_PAD.encode(tag),
        URL_SAFE_NO_PAD.encode(buf)
    ))
}

/// Decrypt a machine-bound AES-256-GCM blob produced by `seal_machine` or Electron `account.js`.
#[must_use]
pub fn open_machine(data: &str, hwid: &str) -> Option<String> {
    let mut parts = data.split('.');
    let iv = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    let tag = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    let mut ct = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    if parts.next().is_some() || iv.len() != 12 || tag.len() != 16 {
        return None;
    }
    let key = machine_key(hwid);
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    cipher
        .decrypt_in_place_detached(Nonce::from_slice(&iv), b"", &mut ct, Tag::from_slice(&tag))
        .ok()?;
    String::from_utf8(ct).ok()
}

fn state_file_in(app_dir: &Path) -> PathBuf {
    app_dir.join("account.json")
}

fn read_state_raw(app_dir: &Path) -> AccountStateFile {
    std::fs::read_to_string(state_file_in(app_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_state(app_dir: &Path, state: &AccountStateFile) {
    let file = state_file_in(app_dir);
    let tmp = app_dir.join(format!("account.json.tmp-{}", std::process::id()));
    if let Ok(json) = serde_json::to_string(state) {
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
            let _ = std::fs::rename(&tmp, &file);
        }
    }
}

fn clear_state_in(app_dir: &Path) {
    if let Ok(mut mem) = memory_secrets().lock() {
        mem.refresh.clear();
        mem.device_pem.clear();
    }
    let _ = std::fs::remove_file(state_file_in(app_dir));
}

fn needs_relogin(state: &AccountStateFile) -> bool {
    let Some(ref refresh) = state.refresh else {
        return false;
    };
    let Some(ref device) = state.device else {
        return true;
    };
    refresh.kind == "plain" || device.kind == "plain"
}

fn settle_saved_login(app_dir: &Path) {
    let should_run = memory_secrets()
        .lock()
        .map(|mut m| {
            if m.settled {
                false
            } else {
                m.settled = true;
                true
            }
        })
        .unwrap_or(false);
    if !should_run {
        return;
    }
    let mut state = read_state_raw(app_dir);
    if needs_relogin(&state) {
        return;
    }
    if state.remember_until == 0 && state.refresh.is_some() && state.device.is_some() {
        state.remember_until = remember_until_for(now_sec(), true);
        write_state(app_dir, &state);
    } else if state.remember_until == 0 && state.refresh.is_none() && !state.session.is_empty() {
        clear_state_in(app_dir);
    }
}

fn read_state(app_dir: &Path) -> AccountStateFile {
    settle_saved_login(app_dir);
    let state = read_state_raw(app_dir);
    if needs_relogin(&state) {
        clear_state_in(app_dir);
        return AccountStateFile::default();
    }
    if state.remember_until > 0 && now_sec() >= state.remember_until {
        clear_state_in(app_dir);
        return AccountStateFile::default();
    }
    state
}

fn open_secret(blob: Option<&SecretBlob>, hwid: &str) -> String {
    let Some(blob) = blob else {
        return String::new();
    };
    if blob.kind == "machine" {
        open_machine(&blob.data, hwid).unwrap_or_default()
    } else {
        String::new()
    }
}

fn current_refresh_token(state: &AccountStateFile, hwid: &str) -> String {
    if let Ok(mem) = memory_secrets().lock() {
        if !mem.refresh.is_empty() {
            return mem.refresh.clone();
        }
    }
    open_secret(state.refresh.as_ref(), hwid)
}

fn current_device_pem(state: &AccountStateFile, hwid: &str) -> String {
    if let Ok(mem) = memory_secrets().lock() {
        if !mem.device_pem.is_empty() {
            return mem.device_pem.clone();
        }
    }
    open_secret(state.device.as_ref(), hwid)
}

/// Verified claims of the active session on this machine, or `None`.
#[must_use]
pub fn current_claims() -> Option<SessionClaims> {
    let state = read_state(&app_data_dir());
    if state.session.is_empty() {
        return None;
    }
    verify_session(&state.session, cached_hwid(), now_sec())
}

/// Active account numeric UID, or `None` when logged out.
#[must_use]
pub fn session_uid() -> Option<i64> {
    current_claims().map(|c| c.uid)
}

/// Effective maximum clone count (`0` when logged out).
#[must_use]
pub fn effective_max() -> usize {
    current_claims().map_or(0, |c| c.max)
}

/// Ensure the user is logged in to ZaloMulti.
///
/// # Errors
///
/// Returns [`AccountError::Local`] with [`NOT_LOGGED_IN`] when no valid session is active.
pub fn require_logged_in() -> Result<(), AccountError> {
    if effective_max() == 0 {
        Err(AccountError::Local(NOT_LOGGED_IN.into()))
    } else {
        Ok(())
    }
}

/// Snapshot current ZaloMulti account status for the UI.
#[must_use]
pub fn status() -> AccountStatus {
    let app_dir = app_data_dir();
    let state = read_state(&app_dir);
    let hwid = cached_hwid();
    let claims = if state.session.is_empty() {
        None
    } else {
        verify_session(&state.session, hwid, now_sec())
    };
    let has_refresh = !current_refresh_token(&state, hwid).is_empty();
    AccountStatus {
        logged_in: claims.is_some(),
        email: state.email,
        uid: claims.as_ref().map(|c| c.uid),
        max_profiles: claims.as_ref().map_or(0, |c| c.max),
        donated: claims.as_ref().is_some_and(|c| c.donated),
        session_exp: claims.as_ref().map_or(0, |c| c.exp),
        has_refresh,
        hwid_display: format_hwid(hwid),
    }
}

fn api_base() -> String {
    std::env::var("ZALOMULTI_API")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_owned())
}

fn post_json(pathname: &str, body: &serde_json::Value) -> Result<(u16, serde_json::Value), AccountError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| AccountError::Local(e.to_string()))?;
    let url = format!("{}{pathname}", api_base());
    rt.block_on(async move {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent("ZaloMulti-Linux")
            .build()
            .map_err(|e| AccountError::Local(e.to_string()))?;
        let resp = client
            .post(&url)
            .header("X-ZM-Build", "linux-3.0.5-rust")
            .json(body)
            .send()
            .await
            .map_err(|_| {
                AccountError::Server(
                    "Không kết nối được máy chủ tài khoản. Kiểm tra mạng rồi thử lại.".into(),
                )
            })?;
        let status = resp.status().as_u16();
        let data: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({}));
        Ok((status, data))
    })
}

/// Sign in to `https://api.lza.lol/zalomulti/auth/login` (blocking — call off UI thread).
///
/// # Errors
///
/// Returns [`AccountError`] when credentials fail, network fails, or signature is invalid.
pub fn login(email: &str, password: &str, remember: bool) -> Result<AccountStatus, AccountError> {
    let app_dir = app_data_dir();
    settle_saved_login(&app_dir);
    let email = email.trim().to_ascii_lowercase();
    if email.is_empty() || password.is_empty() {
        return Err(AccountError::Local(
            "Vui lòng nhập đầy đủ email và mật khẩu.".into(),
        ));
    }
    let (device_pem, device_key) = generate_device_keypair()?;
    let hwid = cached_hwid();
    let payload = serde_json::json!({
        "email": email,
        "password": password,
        "hwid": hwid,
        "platform": PLATFORM,
        "deviceKey": device_key,
    });
    let (code, data) = post_json("/auth/login", &payload)?;
    if !(200..300).contains(&code) || data.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        let msg = data
            .get("error")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Máy chủ trả lỗi {code}."));
        return Err(AccountError::Server(msg));
    }
    let session = data
        .get("session")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    let refresh_tok = data
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    if verify_session(&session, hwid, now_sec()).is_none() {
        return Err(AccountError::Server(
            "Phiên đăng nhập máy chủ trả về không hợp lệ.".into(),
        ));
    }

    let (refresh_blob, device_blob) = if remember && !refresh_tok.is_empty() {
        match (
            seal_machine(&refresh_tok, hwid),
            seal_machine(&device_pem, hwid),
        ) {
            (Ok(r), Ok(d)) => (
                Some(SecretBlob {
                    kind: "machine".into(),
                    data: r,
                }),
                Some(SecretBlob {
                    kind: "machine".into(),
                    data: d,
                }),
            ),
            _ => (None, None),
        }
    } else {
        (None, None)
    };
    let saved = refresh_blob.is_some() && device_blob.is_some();
    if let Ok(mut mem) = memory_secrets().lock() {
        mem.refresh = if saved { String::new() } else { refresh_tok };
        mem.device_pem = if saved { String::new() } else { device_pem };
    }
    let state = AccountStateFile {
        email,
        session,
        remember_until: if saved {
            remember_until_for(now_sec(), true)
        } else {
            0
        },
        refresh: refresh_blob,
        device: device_blob,
    };
    write_state(&app_dir, &state);
    Ok(status())
}

/// Refresh the `zms2` session token using the stored refresh token + Ed25519 proof
/// (blocking — call off UI thread).
///
/// # Errors
///
/// Returns [`AccountError`] when the session is expired and refresh fails.
pub fn refresh() -> Result<AccountStatus, AccountError> {
    let app_dir = app_data_dir();
    let state = read_state(&app_dir);
    let hwid = cached_hwid();
    let token = current_refresh_token(&state, hwid);
    if token.is_empty() {
        return Ok(status());
    }
    let pem = current_device_pem(&state, hwid);
    let mut body = serde_json::json!({
        "refreshToken": token,
        "hwid": hwid,
        "platform": PLATFORM,
    });
    if !pem.is_empty() {
        if let Ok(proof) = device_proof(&pem, hwid, &token, now_sec()) {
            body["proof"] = serde_json::json!({ "ts": proof.ts, "sig": proof.sig });
        }
    }
    let res = post_json("/auth/session", &body);
    match res {
        Ok((code, data))
            if (200..300).contains(&code)
                && data.get("ok").and_then(|v| v.as_bool()) != Some(false) =>
        {
            if let Some(session) = data.get("session").and_then(|v| v.as_str()) {
                if verify_session(session, hwid, now_sec()).is_some() {
                    let mut next = read_state(&app_dir);
                    next.session = session.to_owned();
                    write_state(&app_dir, &next);
                }
            }
            Ok(status())
        }
        Ok((401, data)) => {
            let err_msg = data
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let proof_rejected = err_msg.contains("Không xác minh được thiết bị");
            let still_valid = verify_session(&state.session, hwid, now_sec()).is_some();
            if !proof_rejected || !still_valid {
                clear_state_in(&app_dir);
            }
            Ok(status())
        }
        Ok((code, data)) => {
            if verify_session(&state.session, hwid, now_sec()).is_some() {
                Ok(status())
            } else {
                let msg = data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Máy chủ trả lỗi {code}."));
                Err(AccountError::Server(msg))
            }
        }
        Err(err) => {
            if verify_session(&state.session, hwid, now_sec()).is_some() {
                Ok(status())
            } else {
                Err(err)
            }
        }
    }
}

/// Sign out of ZaloMulti and revoke the refresh token best-effort (blocking).
pub fn logout() -> AccountStatus {
    let app_dir = app_data_dir();
    let state = read_state(&app_dir);
    let token = current_refresh_token(&state, cached_hwid());
    clear_state_in(&app_dir);
    if !token.is_empty() {
        let _ = post_json("/auth/logout", &serde_json::json!({ "refreshToken": token }));
    }
    status()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign_test_token(pair: &Ed25519KeyPair, payload: &str) -> String {
        let sig = pair.sign(payload.as_bytes());
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload.as_bytes()),
            URL_SAFE_NO_PAD.encode(sig.as_ref())
        )
    }

    #[test]
    fn verifies_zms2_linux_and_zms1_while_rejecting_wrong_platform_or_expired() {
        let (pem, pub_b64) = generate_device_keypair().expect("keypair");
        let pair = parse_ed25519_pem(&pem).expect("parse pem");
        let hwid = "a".repeat(64);
        let now = 1_700_000_000;
        let exp = now + 600;

        let zms2 = sign_test_token(&pair, &format!("zms2|42|{hwid}|4|1|{exp}|linux"));
        let claims = verify_session_with_key(&zms2, &hwid, now, &pub_b64).expect("valid zms2");
        assert_eq!(claims.uid, 42);
        assert_eq!(claims.max, 4);
        assert!(claims.donated);

        let zms1 = sign_test_token(&pair, &format!("zms1|42|{hwid}|4|0|{exp}"));
        let claims1 = verify_session_with_key(&zms1, &hwid, now, &pub_b64).expect("valid zms1");
        assert_eq!(claims1.max, 4);
        assert!(!claims1.donated);

        let wrong_os = sign_test_token(&pair, &format!("zms2|42|{hwid}|4|0|{exp}|windows"));
        assert!(verify_session_with_key(&wrong_os, &hwid, now, &pub_b64).is_none());

        let expired = sign_test_token(&pair, &format!("zms2|42|{hwid}|4|0|{now}|linux"));
        assert!(verify_session_with_key(&expired, &hwid, now, &pub_b64).is_none());

        let wrong_hwid = sign_test_token(&pair, &format!("zms2|42|{}|4|0|{exp}|linux", "b".repeat(64)));
        assert!(verify_session_with_key(&wrong_hwid, &hwid, now, &pub_b64).is_none());
    }

    #[test]
    fn device_proof_matches_server_zmr1_contract() {
        let (pem, pub_b64) = generate_device_keypair().expect("keypair");
        let hwid = "h".repeat(64);
        let proof = device_proof(&pem, &hwid, "refresh-tok", 1234).expect("proof");
        assert_eq!(proof.ts, 1234);

        let pub_bytes = URL_SAFE_NO_PAD.decode(&pub_b64).unwrap();
        let sig_bytes = URL_SAFE_NO_PAD.decode(&proof.sig).unwrap();
        let hash = hex::encode(Sha256::digest(b"refresh-tok"));
        let expected_msg = format!("zmr1|{hwid}|1234|{hash}");
        let verifier = UnparsedPublicKey::new(&ED25519, &pub_bytes);
        assert!(verifier.verify(expected_msg.as_bytes(), &sig_bytes).is_ok());
        assert!(verifier.verify(b"zmr1|other", &sig_bytes).is_err());
    }

    #[test]
    fn machine_seal_and_open_roundtrip_and_bind_to_hwid() {
        let hwid = "c".repeat(64);
        let sealed = seal_machine("secret-refresh-token", &hwid).expect("seal");
        assert_eq!(sealed.split('.').count(), 3);
        assert_eq!(open_machine(&sealed, &hwid).as_deref(), Some("secret-refresh-token"));
        assert_eq!(open_machine(&sealed, &"d".repeat(64)), None);
    }

    #[test]
    fn remember_window_is_exact_15_days() {
        let now = 1_700_000_000;
        let until = remember_until_for(now, true);
        assert_eq!(until - now, 15 * 24 * 60 * 60);
        assert!(saved_login_active(true, until, now));
        assert!(saved_login_active(true, until, until - 1));
        assert!(!saved_login_active(true, until, until));
        assert!(!saved_login_active(true, remember_until_for(now, false), now));
        assert!(!saved_login_active(false, until, now));
    }
}
