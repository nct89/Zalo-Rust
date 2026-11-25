//! Real QR login, same steps as `zalo-gui`'s `login_worker`:
//! `getServerInfo` → `request_qr` → `wait_for_scan` → `wait_for_confirm` →
//! save `session.json` → `getLoginInfo` → save again.
//!
//! Runs on its own thread with a current-thread tokio runtime and reports
//! [`LoginEvent`]s through an unbounded channel the GPUI view drains.

use futures::channel::mpsc::UnboundedSender;
use zalo_client::{
    confirm_zpw_sek, ClientError, Device, QrScanOutcome, StoredSession, ZaloApi,
};

/// Payload rendered as a QR code in [`LoginEvent::Qr`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrPrompt {
    /// QR text/URL payload (`qr_cnt` / `ntk`), empty if only `base64_png` was returned.
    pub url: String,
    /// Optional server-rendered base64 PNG (`base64_qr`).
    pub base64_png: Option<String>,
    /// Validity in seconds.
    pub timeout_seconds: u64,
    /// True when the server issued a second security-verification QR (`error_code == -20`).
    pub security_step: bool,
    /// Optional server notice explaining the security verification step.
    pub message: Option<String>,
}

/// Progress of one login attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginEvent {
    /// Human-readable step.
    Status(String),
    /// QR code to display (initial or `-20` security verification renewal).
    Qr(QrPrompt),
    /// Phone scanned; waiting for "Confirm" on the phone.
    Scanned(Option<String>),
    /// QR timed out (show the reload overlay).
    Expired,
    /// Session saved; open the main window.
    Done,
    /// Hard failure.
    Failed(String),
}

/// Start one attempt on a background thread.
pub fn start(tx: UnboundedSender<LoginEvent>) {
    let _ = std::thread::Builder::new()
        .name("zalo-gpui-login".into())
        .spawn(move || {
            let send = |e: LoginEvent| {
                let _ = tx.unbounded_send(e);
            };
            match run(&send) {
                Ok(true) => send(LoginEvent::Done),
                Ok(false) => send(LoginEvent::Expired),
                Err(err) => send(LoginEvent::Failed(err)),
            }
        });
}

/// `Ok(true)` logged in, `Ok(false)` QR expired.
fn run(send: &dyn Fn(LoginEvent)) -> Result<bool, String> {
    let dir = zalo_core::AppPaths::from_env("zalo-rust")
        .map_err(|e| e.to_string())?
        .config_dir()
        .to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Wait for any in-flight server `/api/v2/login/logOut` on this device IMEI
    // before requesting a new QR ticket so the old session is cleanly closed
    // and cannot race with or invalidate the new QR session.
    send(LoginEvent::Status("Đang kết nối máy chủ Zalo…".into()));
    let _ = crate::bridge::wait_pending_logout();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        // Reuses device.json so the IMEI stays stable across logins.
        let device = Device::load_or_create(&dir).map_err(|e| e.to_string())?;
        let api = ZaloApi::new(device).map_err(|e| e.to_string())?;
        api.get_server_info()
            .await
            .map_err(|e| format!("getServerInfo: {e}"))?;
        send(LoginEvent::Status("Đang lấy mã QR…".into()));
        let mut ticket = api.request_qr().await.map_err(|e| format!("reqqr: {e}"))?;
        eprintln!(
            "zalo-gpui login: reqqr ok (timeout={}s, url_len={}, has_b64={})",
            ticket.timeout_seconds,
            ticket.qr_url.len(),
            ticket.base64_qr.is_some()
        );
        send(LoginEvent::Qr(QrPrompt {
            url: ticket.qr_url.clone(),
            base64_png: ticket.base64_qr.clone(),
            timeout_seconds: ticket.timeout_seconds,
            security_step: false,
            message: None,
        }));
        let mut renewals = 0u8;
        loop {
            match api.wait_for_scan_step(&ticket).await {
                Ok(QrScanOutcome::Scanned(v)) => {
                    let name = v
                        .pointer("/data/display_name")
                        .or_else(|| v.pointer("/data/displayName"))
                        .and_then(|x| x.as_str())
                        .map(str::to_owned);
                    eprintln!("zalo-gpui login: qr scanned");
                    send(LoginEvent::Scanned(name));
                    break;
                }
                Ok(QrScanOutcome::RenewQr {
                    ticket: next_ticket,
                    message,
                }) => {
                    renewals = renewals.saturating_add(1);
                    eprintln!("zalo-gpui login: -20 security QR renewal #{renewals}");
                    if renewals > 4 {
                        return Err("Xác nhận bảo mật thất bại quá nhiều lần. Hãy thử tải lại mã QR.".into());
                    }
                    ticket = next_ticket;
                    let note = if message.trim().is_empty() {
                        None
                    } else {
                        Some(message)
                    };
                    send(LoginEvent::Qr(QrPrompt {
                        url: ticket.qr_url.clone(),
                        base64_png: ticket.base64_qr.clone(),
                        timeout_seconds: ticket.timeout_seconds,
                        security_step: true,
                        message: note,
                    }));
                }
                Err(ClientError::Timeout(_)) => {
                    eprintln!("zalo-gpui login: wait_for_scan timeout");
                    return Ok(false);
                }
                Err(ClientError::Zalo { code: 18010, .. }) => {
                    eprintln!("zalo-gpui login: wait_for_scan rejected (18010)");
                    return Err("Bạn đã từ chối đăng nhập trên điện thoại.".into());
                }
                Err(e) => {
                    eprintln!("zalo-gpui login: wait_for_scan error: {e}");
                    return Err(format!("wait_for_scan: {e}"));
                }
            }
        }
        let confirm_val = match api.wait_for_confirm(&ticket).await {
            Ok(v) => v,
            Err(ClientError::Timeout(_)) => {
                eprintln!("zalo-gpui login: wait_for_confirm timeout");
                return Ok(false);
            }
            Err(ClientError::Zalo { code: 18010, .. }) => {
                eprintln!("zalo-gpui login: wait_for_confirm rejected (18010)");
                return Err("Bạn đã từ chối đăng nhập trên điện thoại.".into());
            }
            Err(e) => {
                eprintln!("zalo-gpui login: wait_for_confirm error: {e}");
                return Err(format!("wait_for_confirm: {e}"));
            }
        };
        send(LoginEvent::Status("Đang lưu phiên đăng nhập…".into()));
        let mut session = StoredSession::from_cookie_headers(api.cookie_headers());
        if let Some(sek) = confirm_zpw_sek(&confirm_val) {
            session.ensure_zpw_sek(&sek);
        }
        let device = Device::load_or_create(&dir).map_err(|e| e.to_string())?;
        let mut api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
        send(LoginEvent::Status("Đang lấy thông tin tài khoản…".into()));
        let info = api
            .get_login_info()
            .await
            .map_err(|e| format!("getLoginInfo: {e}"))?;
        // The account id is profile/me userId. dkey and send2me_id are not.
        // Saving before this returns would keep the previous account on screen.
        let me = api
            .get_me()
            .await
            .map_err(|e| format!("profile/me: {e}"))?;
        let account = crate::account::me_from_value(&me);
        let user_id = account.uid.trim().to_owned();
        if user_id.is_empty() {
            return Err(
                "Máy chủ không trả userId của tài khoản vừa quét. Phiên chưa được lưu.".into(),
            );
        }
        let name = if account.display_name.trim().is_empty() {
            info.display_name.clone()
        } else {
            Some(account.display_name.clone())
        };
        session.apply_login_info(
            info.uin.clone(),
            info.zpw_enk.clone(),
            info.phone_number.clone(),
            name,
        );
        session.send2me_id = info.send2me_id.clone();
        session.user_id = Some(user_id.clone());
        let dest = crate::data::account_dir(&dir, &user_id);
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        crate::data::share_device(&dir, &dest).map_err(|e| e.to_string())?;
        let prev = crate::data::read_current_user(&dir);
        let switched_account = prev.as_deref() != Some(user_id.as_str());
        if switched_account {
            crate::data::migrate_legacy_store(&dir, &dest, &user_id);
        }
        crate::background::stop();
        crate::sync_worker::reset_sync_identity_key();
        crate::bridge::clear_login_cache();
        session.save(&dest).map_err(|e| e.to_string())?;
        if dest != dir {
            crate::data::wipe_session_file(&dir);
        }
        crate::data::write_current_user(&dir, &user_id);
        crate::data::write_last_account_uid(&dest, &user_id);
        // App prefs stay on the machine dir. Account-scoped prefs live under `dest`.
        Ok(true)
    })
}

/// True when an API error means this PC session is dead.
///
/// Electron logs out on 102 (session timeout), 600 (`ERR_KICK_OUT`), and
/// polling `-69`. A body that cannot be decrypted without `zpw_sek` is the
/// same dead session. Close codes 3000 and 3003 are not this check: 3000 is
/// the socket duplicate command, and 3003 is retried.
#[must_use]
pub fn is_session_error(err: &str) -> bool {
    err.contains("returned 102")
        || err.contains("returned 600")
        || err.contains("returned -69")
        || err.contains("zpw_sek")
        || (err.contains("session I/O failed")
            && (err.contains("No such file") || err.contains("os error 2")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_errors_are_detected() {
        assert!(is_session_error(
            "login_info: Zalo returned 102: The session key …"
        ));
        assert!(is_session_error(
            "friends: Zalo returned 600: phiên đã kết thúc"
        ));
        assert!(is_session_error("poll: Zalo returned -69: logout"));
        assert!(is_session_error(
            "session I/O failed: No such file or directory (os error 2)"
        ));
        assert!(!is_session_error("timeout"));
        assert!(!is_session_error("close 3003"));
    }
}
