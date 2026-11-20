//! Real QR login, same steps as `zalo-gui`'s `login_worker`:
//! `getServerInfo` → `request_qr` → `wait_for_scan` → `wait_for_confirm` →
//! save `session.json` → `getLoginInfo` → save again.
//!
//! Runs on its own thread with a current-thread tokio runtime and reports
//! [`LoginEvent`]s through an unbounded channel the GPUI view drains.

use futures::channel::mpsc::UnboundedSender;
use zalo_client::{ClientError, Device, StoredSession, ZaloApi};

/// Progress of one login attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginEvent {
    /// Human-readable step.
    Status(String),
    /// URL to encode in the QR and its validity.
    Qr(String, u64),
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
    let dir = zalo_core::XdgDirs::from_env()
        .map_err(|e| e.to_string())?
        .config_home()
        .join("zalo-rust");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        // Reuses device.json so the IMEI stays stable across logins.
        let device = Device::load_or_create(&dir).map_err(|e| e.to_string())?;
        let api = ZaloApi::new(device).map_err(|e| e.to_string())?;
        send(LoginEvent::Status("Đang kết nối máy chủ Zalo…".into()));
        api.get_server_info()
            .await
            .map_err(|e| format!("getServerInfo: {e}"))?;
        send(LoginEvent::Status("Đang lấy mã QR…".into()));
        let ticket = api.request_qr().await.map_err(|e| format!("reqqr: {e}"))?;
        send(LoginEvent::Qr(
            ticket.qr_url.clone(),
            ticket.timeout_seconds,
        ));
        match api.wait_for_scan(&ticket).await {
            Ok(v) => {
                let name = v
                    .pointer("/data/display_name")
                    .or_else(|| v.pointer("/data/displayName"))
                    .and_then(|x| x.as_str())
                    .map(str::to_owned);
                send(LoginEvent::Scanned(name));
            }
            Err(ClientError::Timeout(_)) => return Ok(false),
            Err(e) => return Err(format!("wait_for_scan: {e}")),
        }
        match api.wait_for_confirm(&ticket).await {
            Ok(_) => {}
            Err(ClientError::Timeout(_)) => return Ok(false),
            Err(e) => return Err(format!("wait_for_confirm: {e}")),
        }
        send(LoginEvent::Status("Đang lưu phiên đăng nhập…".into()));
        let session = StoredSession::from_cookie_headers(api.cookie_headers());
        session.save(&dir).map_err(|e| e.to_string())?;
        let device = Device::load_or_create(&dir).map_err(|e| e.to_string())?;
        let mut api = ZaloApi::from_session(device, &session).map_err(|e| e.to_string())?;
        send(LoginEvent::Status("Đang lấy thông tin tài khoản…".into()));
        let info = api
            .get_login_info()
            .await
            .map_err(|e| format!("getLoginInfo: {e}"))?;
        let me = api.get_me().await.ok();
        let name = info.display_name.clone().or_else(|| {
            me.as_ref()
                .and_then(|v| crate::profiles::find_str(v, &["displayName", "zaloName"]))
        });
        let me_uid = me
            .as_ref()
            .map(|v| crate::account::me_from_value(v).uid)
            .filter(|uid| !uid.is_empty());
        let mut session = session;
        session.apply_login_info(
            info.uin.clone(),
            info.zpw_enk.clone(),
            info.phone_number.clone(),
            name,
        );
        if let Some(uid) = info.send2me_id.clone().or(me_uid) {
            session.send2me_id = Some(uid);
        }
        session.save(&dir).map_err(|e| e.to_string())?;
        Ok(true)
    })
}

/// True when an API error means the saved session is no longer valid
/// (102 session timeout, 600 missing / wrong `zpw_sek`).
#[must_use]
pub fn is_session_error(err: &str) -> bool {
    err.contains("returned 102") || err.contains("returned 600") || err.contains("zpw_sek")
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
            "friends: Zalo returned 600: zpw_sek bị thiếu"
        ));
        assert!(!is_session_error("timeout"));
    }
}
