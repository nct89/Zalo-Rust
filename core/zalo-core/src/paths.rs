//! XDG base directories and the user's download folder.
//!
//! The rules follow the XDG Base Directory Specification: an environment
//! variable is used only when it holds an absolute path, otherwise the
//! default under $HOME applies. From `user-dirs.dirs` only the
//! `XDG_*_DIR` entries are read, with $HOME and ${HOME} expanded.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// An error while building [`XdgDirs`] from the process environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathError {
    /// $HOME is unset or empty.
    MissingHome,
    /// $HOME is not an absolute path.
    RelativeHome(PathBuf),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHome => f.write_str("$HOME is not set"),
            Self::RelativeHome(home) => write!(f, "$HOME is not absolute: {}", home.display()),
        }
    }
}

impl std::error::Error for PathError {}

/// The XDG directories of one Linux user.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XdgDirs {
    home: PathBuf,
    config_home: PathBuf,
    data_home: PathBuf,
    cache_home: PathBuf,
    state_home: PathBuf,
    runtime_dir: Option<PathBuf>,
}

impl XdgDirs {
    /// Builds the directories from the current process environment.
    ///
    /// # Errors
    ///
    /// Returns [`PathError`] when $HOME is missing, empty or relative.
    pub fn from_env() -> Result<Self, PathError> {
        let home = env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .ok_or(PathError::MissingHome)?;
        if !home.is_absolute() {
            return Err(PathError::RelativeHome(home));
        }
        Ok(Self::from_lookup(home, env_var))
    }

    /// Builds the default directories for `home`, ignoring the environment.
    ///
    /// `home` should be absolute; passing a relative path produces relative
    /// results. Use [`XdgDirs::from_env`] to honour the XDG variables of the
    /// running process.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            config_home: home.join(".config"),
            data_home: home.join(".local").join("share"),
            cache_home: home.join(".cache"),
            state_home: home.join(".local").join("state"),
            runtime_dir: None,
            home,
        }
    }

    /// The user's home directory.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// $XDG_CONFIG_HOME, defaulting to $HOME/.config.
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    /// $XDG_DATA_HOME, defaulting to $HOME/.local/share.
    pub fn data_home(&self) -> &Path {
        &self.data_home
    }

    /// $XDG_CACHE_HOME, defaulting to $HOME/.cache.
    pub fn cache_home(&self) -> &Path {
        &self.cache_home
    }

    /// $XDG_STATE_HOME, defaulting to $HOME/.local/state.
    pub fn state_home(&self) -> &Path {
        &self.state_home
    }

    /// $XDG_RUNTIME_DIR, or `None` when it is unset or not absolute.
    pub fn runtime_dir(&self) -> Option<&Path> {
        self.runtime_dir.as_deref()
    }

    /// Reads one `XDG_*_DIR` entry from `user-dirs.dirs` for `key`.
    ///
    /// Returns `None` when the file, the key or a valid absolute path is
    /// missing. $HOME and ${HOME} in the value are expanded.
    pub fn user_dir(&self, key: &str) -> Option<PathBuf> {
        let file = self.config_home.join("user-dirs.dirs");
        let content = fs::read_to_string(file).ok()?;
        parse_user_dirs(&content, &self.home).remove(key)
    }

    /// The user's download folder: `XDG_DOWNLOAD_DIR` when set, otherwise
    /// $HOME/Downloads. The folder is not created.
    pub fn downloads_dir(&self) -> PathBuf {
        self.user_dir("XDG_DOWNLOAD_DIR")
            .unwrap_or_else(|| self.home.join("Downloads"))
    }

    fn from_lookup(home: PathBuf, lookup: impl Fn(&str) -> Option<OsString>) -> Self {
        let mut dirs = Self::new(home);
        dirs.config_home = absolute_dir(lookup("XDG_CONFIG_HOME")).unwrap_or(dirs.config_home);
        dirs.data_home = absolute_dir(lookup("XDG_DATA_HOME")).unwrap_or(dirs.data_home);
        dirs.cache_home = absolute_dir(lookup("XDG_CACHE_HOME")).unwrap_or(dirs.cache_home);
        dirs.state_home = absolute_dir(lookup("XDG_STATE_HOME")).unwrap_or(dirs.state_home);
        dirs.runtime_dir = absolute_dir(lookup("XDG_RUNTIME_DIR"));
        dirs
    }
}

/// Cross-platform application directories for Linux (XDG), Windows (`APPDATA` /
/// `LOCALAPPDATA`), and macOS (`~/Library/Application Support` / `Caches`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    home: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    downloads_dir: PathBuf,
}

impl AppPaths {
    /// Resolves standard per-user directories for `app_name` (e.g. `"zalo-rust"`)
    /// from the current OS environment.
    ///
    /// # Errors
    ///
    /// Returns [`PathError`] when the user's home directory cannot be resolved.
    pub fn from_env(app_name: &str) -> Result<Self, PathError> {
        #[cfg(target_os = "windows")]
        {
            Self::from_windows_lookup(app_name, env_var)
        }
        #[cfg(target_os = "macos")]
        {
            Self::from_macos_lookup(app_name, env_var)
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let xdg = XdgDirs::from_env()?;
            Ok(Self {
                config_dir: xdg.config_home().join(app_name),
                data_dir: xdg.data_home().join(app_name),
                cache_dir: xdg.cache_home().join(app_name),
                downloads_dir: xdg.downloads_dir(),
                home: xdg.home().to_path_buf(),
            })
        }
    }

    /// Resolves Windows directory layout from `lookup` (`USERPROFILE`, `APPDATA`, `LOCALAPPDATA`).
    ///
    /// # Errors
    ///
    /// Returns [`PathError`] when neither `USERPROFILE` nor `HOME` is set to an absolute path.
    pub fn from_windows_lookup(
        app_name: &str,
        lookup: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, PathError> {
        let home = lookup("USERPROFILE")
            .filter(|v| !v.is_empty())
            .or_else(|| lookup("HOME").filter(|v| !v.is_empty()))
            .map(PathBuf::from)
            .ok_or(PathError::MissingHome)?;
        if !home.is_absolute() {
            return Err(PathError::RelativeHome(home));
        }
        let roaming = absolute_dir(lookup("APPDATA"))
            .unwrap_or_else(|| home.join("AppData").join("Roaming"));
        let local = absolute_dir(lookup("LOCALAPPDATA"))
            .unwrap_or_else(|| home.join("AppData").join("Local"));
        let app_dir = roaming.join(app_name);
        Ok(Self {
            config_dir: app_dir.clone(),
            data_dir: app_dir,
            cache_dir: local.join(app_name).join("Cache"),
            downloads_dir: home.join("Downloads"),
            home,
        })
    }

    /// Resolves macOS directory layout from `lookup` (`HOME`).
    ///
    /// # Errors
    ///
    /// Returns [`PathError`] when `HOME` is missing or relative.
    pub fn from_macos_lookup(
        app_name: &str,
        lookup: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, PathError> {
        let home = lookup("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .ok_or(PathError::MissingHome)?;
        if !home.is_absolute() {
            return Err(PathError::RelativeHome(home));
        }
        let support = home
            .join("Library")
            .join("Application Support")
            .join(app_name);
        Ok(Self {
            config_dir: support.clone(),
            data_dir: support,
            cache_dir: home.join("Library").join("Caches").join(app_name),
            downloads_dir: home.join("Downloads"),
            home,
        })
    }

    /// The user's home directory.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Directory for configuration files, `session.json`, and `store.db`.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Directory for persistent app data (OCR models, WASM, extracted backups).
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Directory for media/thumbnail/profile caches.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// The user's Downloads folder.
    pub fn downloads_dir(&self) -> &Path {
        &self.downloads_dir
    }
}

fn env_var(key: &str) -> Option<OsString> {
    env::var_os(key)
}

fn absolute_dir(value: Option<OsString>) -> Option<PathBuf> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

fn parse_user_dirs(content: &str, home: &Path) -> BTreeMap<String, PathBuf> {
    let mut dirs = BTreeMap::new();
    for line in content.lines() {
        let Some((key, value)) = parse_user_dir_line(line) else {
            continue;
        };
        let Some(path) = expand_home(&value, home) else {
            continue;
        };
        if path.is_absolute() {
            dirs.insert(key, path);
        }
    }
    dirs
}

fn parse_user_dir_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if !key.starts_with("XDG_") {
        return None;
    }
    let value = unquote(value.trim())?;
    Some((key.to_owned(), value))
}

fn unquote(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    if !value.starts_with('"') {
        return Some(value.to_owned());
    }
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

fn expand_home(value: &str, home: &Path) -> Option<PathBuf> {
    let home = home.to_str()?;
    let mut expanded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(position) = rest.find('$') {
        expanded.push_str(&rest[..position]);
        let tail = &rest[position..];
        if let Some(after) = tail.strip_prefix("${HOME}") {
            expanded.push_str(home);
            rest = after;
        } else if let Some(after) = tail.strip_prefix("$HOME") {
            if after.is_empty() || after.starts_with('/') {
                expanded.push_str(home);
                rest = after;
            } else {
                expanded.push('$');
                rest = &tail[1..];
            }
        } else {
            expanded.push('$');
            rest = &tail[1..];
        }
    }
    expanded.push_str(rest);
    Some(PathBuf::from(expanded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn abs_path(parts: &[&str]) -> PathBuf {
        #[cfg(windows)]
        let mut path = PathBuf::from(r"C:\");
        #[cfg(not(windows))]
        let mut path = PathBuf::from("/");
        for part in parts {
            path.push(part);
        }
        path
    }

    #[test]
    fn defaults_follow_the_xdg_specification() {
        let home = abs_path(&["home", "ana"]);
        let dirs = XdgDirs::new(&home);
        assert_eq!(dirs.config_home(), home.join(".config"));
        assert_eq!(dirs.data_home(), home.join(".local").join("share"));
        assert_eq!(dirs.cache_home(), home.join(".cache"));
        assert_eq!(dirs.state_home(), home.join(".local").join("state"));
        assert_eq!(dirs.runtime_dir(), None);
        assert_eq!(dirs.downloads_dir(), home.join("Downloads"));
    }

    #[test]
    fn environment_overrides_must_be_absolute() {
        let home = abs_path(&["home", "ana"]);
        let config = abs_path(&["etc", "zalo", "config"]);
        let state = abs_path(&["var", "state"]);
        let runtime = abs_path(&["run", "user", "1000"]);
        let dirs = XdgDirs::from_lookup(home.clone(), |key| match key {
            "XDG_CONFIG_HOME" => Some(config.clone().into_os_string()),
            "XDG_DATA_HOME" => Some(OsString::from("relative/data")),
            "XDG_CACHE_HOME" => Some(OsString::new()),
            "XDG_STATE_HOME" => Some(state.clone().into_os_string()),
            "XDG_RUNTIME_DIR" => Some(runtime.clone().into_os_string()),
            _ => None,
        });
        assert_eq!(dirs.config_home(), config.as_path());
        assert_eq!(dirs.data_home(), home.join(".local").join("share"));
        assert_eq!(dirs.cache_home(), home.join(".cache"));
        assert_eq!(dirs.state_home(), state.as_path());
        assert_eq!(dirs.runtime_dir(), Some(runtime.as_path()));
    }

    #[test]
    fn reads_user_dirs_and_ignores_invalid_lines() {
        let temp = TempDir::new("user-dirs");
        let config = temp.path().join("config");
        fs::create_dir_all(&config).expect("create config dir");
        fs::write(
            config.join("user-dirs.dirs"),
            concat!(
                "# written by xdg-user-dirs-update\n",
                "XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n",
                "XDG_DOWNLOAD_DIR=\"${HOME}/Tải về\"\n",
                "XDG_DOCUMENTS_DIR=relative/path\n",
                "XDG_MUSIC_DIR=\"$HOME/Music\"\n",
                "not a key/value pair\n",
            ),
        )
        .expect("write user-dirs.dirs");

        let mut dirs = XdgDirs::new(temp.path().join("home"));
        dirs.config_home = config;

        assert_eq!(
            dirs.downloads_dir(),
            temp.path().join("home").join("Tải về")
        );
        assert_eq!(
            dirs.user_dir("XDG_DESKTOP_DIR"),
            Some(temp.path().join("home").join("Desktop"))
        );
        assert_eq!(
            dirs.user_dir("XDG_MUSIC_DIR"),
            Some(temp.path().join("home").join("Music"))
        );
        assert_eq!(dirs.user_dir("XDG_DOCUMENTS_DIR"), None);
        assert_eq!(dirs.user_dir("XDG_VIDEOS_DIR"), None);
    }

    #[test]
    fn app_paths_resolve_macos_layout() {
        let home = abs_path(&["Users", "ana"]);
        let paths = AppPaths::from_macos_lookup("zalo-rust", |key| match key {
            "HOME" => Some(home.clone().into_os_string()),
            _ => None,
        })
        .expect("macos paths");
        assert_eq!(
            paths.config_dir(),
            home.join("Library")
                .join("Application Support")
                .join("zalo-rust")
        );
        assert_eq!(
            paths.cache_dir(),
            home.join("Library").join("Caches").join("zalo-rust")
        );
        assert_eq!(paths.downloads_dir(), home.join("Downloads"));
    }

    #[test]
    fn app_paths_resolve_windows_layout() {
        let home = abs_path(&["Users", "ana"]);
        let roaming = home.join("AppData").join("Roaming");
        let local = home.join("AppData").join("Local");
        let paths = AppPaths::from_windows_lookup("zalo-rust", |key| match key {
            "USERPROFILE" => Some(home.clone().into_os_string()),
            "APPDATA" => Some(roaming.clone().into_os_string()),
            "LOCALAPPDATA" => Some(local.clone().into_os_string()),
            _ => None,
        })
        .expect("windows paths");
        assert_eq!(paths.config_dir(), roaming.join("zalo-rust"));
        assert_eq!(paths.cache_dir(), local.join("zalo-rust").join("Cache"));
        assert_eq!(paths.downloads_dir(), home.join("Downloads"));
    }
}
