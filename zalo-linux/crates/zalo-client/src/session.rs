//! Local session persistence.
//!
//! The cookie jar is written with mode 0600 because `zpw_sek` is a bearer
//! credential. The file is only a cache of the server-side session; no
//! password is stored.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// What survives a restart of the headless client.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StoredSession {
    /// Cookie headers keyed by the URL they apply to.
    pub cookie_headers: BTreeMap<String, String>,
    /// Session bearer key issued at login, if available.
    pub zpw_sek: Option<String>,
    /// Session data key returned by getLoginInfo, if available.
    pub zpw_enk: Option<String>,
    /// Account UIN (dkey) returned by getLoginInfo, if available.
    pub uin: Option<String>,
    /// Phone number returned by getLoginInfo, if available.
    pub phone_number: Option<String>,
    /// Display name returned by getLoginInfo, if available.
    pub display_name: Option<String>,
    /// Logged-in user's own profile / My Cloud ID (`send2me_id`), if available.
    #[serde(default)]
    pub send2me_id: Option<String>,
    /// UNIX seconds when the file was written.
    pub saved_at: u64,
}

/// Why a session could not be read or written.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Reading or writing the file failed.
    #[error("session I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The session file is not valid JSON.
    #[error("session JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

impl StoredSession {
    /// Saves the session as `session.json` in `dir` with mode 0600.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the directory or file cannot be written.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, SessionError> {
        fs::create_dir_all(dir)?;
        let path = session_path(dir);
        let raw = serde_json::to_vec_pretty(self)?;
        fs::write(&path, raw)?;
        set_private_permissions(&path)?;
        Ok(path)
    }

    /// Loads `session.json` from `dir`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the file is missing or invalid.
    pub fn load(dir: &Path) -> Result<Self, SessionError> {
        let raw = fs::read_to_string(session_path(dir))?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Creates a session from the cookie headers currently held by the jar.
    pub fn from_cookie_headers(cookie_headers: BTreeMap<String, String>) -> Self {
        let zpw_sek = extract_zpw_sek(&cookie_headers);
        Self {
            cookie_headers,
            zpw_sek,
            zpw_enk: None,
            uin: None,
            phone_number: None,
            display_name: None,
            send2me_id: None,
            saved_at: unix_now(),
        }
    }

    /// Records the fields returned by getLoginInfo and refreshes the time.
    pub fn apply_login_info(
        &mut self,
        uin: String,
        zpw_enk: String,
        phone: Option<String>,
        display: Option<String>,
    ) {
        self.uin = Some(uin);
        self.zpw_enk = Some(zpw_enk);
        self.phone_number = phone;
        self.display_name = display;
        self.saved_at = unix_now();
    }

    /// Returns the `zpw_sek` cookie value, from the dedicated field or the
    /// saved cookie headers.
    pub fn zpw_sek(&self) -> Option<String> {
        self.zpw_sek
            .clone()
            .or_else(|| extract_zpw_sek(&self.cookie_headers))
    }
}

/// Extracts the `zpw_sek` value from saved Cookie header strings.
pub fn extract_zpw_sek(headers: &BTreeMap<String, String>) -> Option<String> {
    headers.values().find_map(|header| {
        header.split(';').find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == "zpw_sek" && !value.is_empty()).then(|| value.to_owned())
        })
    })
}

/// The path of the persisted session inside `dir`.
pub fn session_path(dir: &Path) -> PathBuf {
    dir.join("session.json")
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), io::Error> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), io::Error> {
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn round_trips_the_session() {
        let temp = TempDir::new("session");
        let mut headers = BTreeMap::new();
        headers.insert(
            "https://chat.zalo.me/".to_owned(),
            "zpw_sek=test".to_owned(),
        );
        let mut session = StoredSession::from_cookie_headers(headers);
        assert_eq!(session.zpw_sek(), Some("test".to_owned()));
        session.apply_login_info("uin".to_owned(), "enk".to_owned(), None, None);
        let path = session.save(temp.path()).expect("save");
        assert!(path.exists());
        let loaded = StoredSession::load(temp.path()).expect("load");
        assert_eq!(loaded.cookie_headers, session.cookie_headers);
        assert_eq!(loaded.uin.as_deref(), Some("uin"));
        assert_eq!(loaded.zpw_sek(), Some("test".to_owned()));
    }

    #[test]
    fn extracts_zpw_sek_from_headers() {
        let mut headers = BTreeMap::new();
        headers.insert(
            "https://zaloapp.com/".to_owned(),
            "_zlang=vi; zpw_sek=abcdef".to_owned(),
        );
        assert_eq!(extract_zpw_sek(&headers), Some("abcdef".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn session_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new("session-mode");
        let path = StoredSession::default().save(temp.path()).expect("save");
        let mode = fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
