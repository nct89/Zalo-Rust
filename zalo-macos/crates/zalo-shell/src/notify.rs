//! User notifications via `osascript` (`display notification`).

use crate::error::ShellError;

/// Text for one notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopNotice {
    /// Application name. This client uses `Zalo`.
    pub app_name: String,
    /// Notification title.
    pub summary: String,
    /// Notification body.
    pub body: String,
    /// Requested timeout in milliseconds. macOS Notification Center owns the real timeout.
    pub expire_ms: i32,
    /// Unused on macOS (the Dock icon is the notification image).
    pub icon_path: Option<String>,
}

/// Arguments kept so the GUI can format a notice without posting it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotifyCall {
    /// Application name.
    pub app_name: String,
    /// Always zero. Notification Center allocates the id.
    pub replaces_id: u32,
    /// Unused.
    pub app_icon: String,
    /// Title.
    pub summary: String,
    /// Body.
    pub body: String,
    /// Requested timeout in milliseconds.
    pub expire_ms: i32,
}

/// Notice for a positive unread count.
#[must_use]
pub fn unread_notice(count: u32) -> Option<DesktopNotice> {
    if count == 0 {
        return None;
    }
    let body = if count > 99 {
        "99+ tin chưa đọc".to_owned()
    } else {
        format!("{count} tin chưa đọc")
    };
    Some(DesktopNotice {
        app_name: "Zalo".to_owned(),
        summary: "Zalo".to_owned(),
        body,
        expire_ms: 5000,
        icon_path: None,
    })
}

/// Map a notice onto [`NotifyCall`].
#[must_use]
pub fn notify_call(notice: &DesktopNotice) -> NotifyCall {
    NotifyCall {
        app_name: notice.app_name.clone(),
        replaces_id: 0,
        app_icon: String::new(),
        summary: notice.summary.clone(),
        body: notice.body.clone(),
        expire_ms: notice.expire_ms,
    }
}

/// Post `notice`. Returns `1` when Notification Center accepted it.
///
/// Clicking the banner activates the app through AppKit.
///
/// # Errors
///
/// Returns [`ShellError`] when `osascript` cannot start.
pub fn notify_desktop(notice: &DesktopNotice) -> Result<u32, ShellError> {
    if cfg!(test) {
        return Ok(1);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = notice;
        return Ok(0);
    }
    #[cfg(target_os = "macos")]
    {
        let title = escape(&notice.summary);
        let body = escape(&notice.body);
        let script = format!(r#"display notification "{body}" with title "{title}""#);
        let status = std::process::Command::new("osascript")
            .args(["-e", &script])
            .status()
            .map_err(|err| ShellError::Bus(err.to_string()))?;
        if !status.success() {
            return Err(ShellError::Bus(format!("display notification {status}")));
        }
        Ok(1)
    }
}

#[cfg(target_os = "macos")]
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_notice_skips_zero() {
        assert!(unread_notice(0).is_none());
        let notice = unread_notice(4).expect("notice");
        assert_eq!(notice.body, "4 tin chưa đọc");
        assert_eq!(notify_call(&notice).summary, "Zalo");
        assert_eq!(unread_notice(120).expect("cap").body, "99+ tin chưa đọc");
    }
}
