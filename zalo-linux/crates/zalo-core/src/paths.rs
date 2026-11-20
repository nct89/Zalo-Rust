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
            data_home: home.join(".local/share"),
            cache_home: home.join(".cache"),
            state_home: home.join(".local/state"),
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

    #[test]
    fn defaults_follow_the_xdg_specification() {
        let dirs = XdgDirs::new("/home/ana");
        assert_eq!(dirs.config_home(), Path::new("/home/ana/.config"));
        assert_eq!(dirs.data_home(), Path::new("/home/ana/.local/share"));
        assert_eq!(dirs.cache_home(), Path::new("/home/ana/.cache"));
        assert_eq!(dirs.state_home(), Path::new("/home/ana/.local/state"));
        assert_eq!(dirs.runtime_dir(), None);
        assert_eq!(dirs.downloads_dir(), Path::new("/home/ana/Downloads"));
    }

    #[test]
    fn environment_overrides_must_be_absolute() {
        let dirs = XdgDirs::from_lookup(PathBuf::from("/home/ana"), |key| match key {
            "XDG_CONFIG_HOME" => Some(OsString::from("/etc/zalo/config")),
            "XDG_DATA_HOME" => Some(OsString::from("relative/data")),
            "XDG_CACHE_HOME" => Some(OsString::new()),
            "XDG_STATE_HOME" => Some(OsString::from("/var/state")),
            "XDG_RUNTIME_DIR" => Some(OsString::from("/run/user/1000")),
            _ => None,
        });
        assert_eq!(dirs.config_home(), Path::new("/etc/zalo/config"));
        assert_eq!(dirs.data_home(), Path::new("/home/ana/.local/share"));
        assert_eq!(dirs.cache_home(), Path::new("/home/ana/.cache"));
        assert_eq!(dirs.state_home(), Path::new("/var/state"));
        assert_eq!(dirs.runtime_dir(), Some(Path::new("/run/user/1000")));
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

        assert_eq!(dirs.downloads_dir(), temp.path().join("home/Tải về"));
        assert_eq!(
            dirs.user_dir("XDG_DESKTOP_DIR"),
            Some(temp.path().join("home/Desktop"))
        );
        assert_eq!(
            dirs.user_dir("XDG_MUSIC_DIR"),
            Some(temp.path().join("home/Music"))
        );
        assert_eq!(dirs.user_dir("XDG_DOCUMENTS_DIR"), None);
        assert_eq!(dirs.user_dir("XDG_VIDEOS_DIR"), None);
    }
}
