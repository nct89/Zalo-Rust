//! Zalo release versions such as `26.10.20`.

use std::cmp::Ordering;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

/// The file at the project root that pins the Zalo release to build.
pub const PINNED_VERSION_FILE: &str = "zalo-version.txt";

/// A dotted numeric release version, for example `26.10.20`.
///
/// Comparison is numeric, not lexicographic (`26.9.99 < 26.10.0`), and
/// missing trailing components count as zero (`26.10 == 26.10.0`).
#[derive(Clone, Debug)]
pub struct Version {
    parts: Vec<u32>,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, part) in self.parts.iter().enumerate() {
            if index > 0 {
                f.write_str(".")?;
            }
            write!(f, "{part}")?;
        }
        Ok(())
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let length = self.parts.len().max(other.parts.len());
        let left = (0..length).map(|index| self.parts.get(index).copied().unwrap_or(0));
        let right = (0..length).map(|index| other.parts.get(index).copied().unwrap_or(0));
        left.cmp(right)
    }
}

impl FromStr for Version {
    type Err = ParseVersionError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ParseVersionError::Empty);
        }
        let mut parts = Vec::new();
        for (index, component) in input.split('.').enumerate() {
            let value =
                component
                    .parse::<u32>()
                    .map_err(|_| ParseVersionError::InvalidComponent {
                        index,
                        component: component.to_owned(),
                    })?;
            parts.push(value);
        }
        Ok(Self { parts })
    }
}

/// Why a version string could not be parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseVersionError {
    /// The input was empty after trimming.
    Empty,
    /// A dot-separated component is not a `u32`.
    InvalidComponent {
        /// Zero-based position of the component.
        index: usize,
        /// The component as written.
        component: String,
    },
}

impl fmt::Display for ParseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("version is empty"),
            Self::InvalidComponent { index, component } => {
                write!(
                    f,
                    "version component {index} is not a number: {component:?}"
                )
            }
        }
    }
}

impl std::error::Error for ParseVersionError {}

/// Why a pinned version file could not be read.
#[derive(Debug)]
pub enum PinnedVersionError {
    /// Reading the file failed.
    Io(io::Error),
    /// The file does not hold a valid version.
    Parse(ParseVersionError),
}

impl fmt::Display for PinnedVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot read pinned version: {error}"),
            Self::Parse(error) => write!(f, "invalid pinned version: {error}"),
        }
    }
}

impl std::error::Error for PinnedVersionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Parse(error) => Some(error),
        }
    }
}

/// Reads and parses a pinned release file such as [`PINNED_VERSION_FILE`].
///
/// # Errors
///
/// Returns [`PinnedVersionError`] when the file cannot be read or does not
/// contain a valid version.
pub fn read_pinned_version(path: impl AsRef<Path>) -> Result<Version, PinnedVersionError> {
    let content = fs::read_to_string(path).map_err(PinnedVersionError::Io)?;
    content.parse().map_err(PinnedVersionError::Parse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::fs;

    fn version(value: &str) -> Version {
        value.parse().expect("valid test version")
    }

    #[test]
    fn parses_and_displays_release_versions() {
        let parsed = version(" 26.10.20\n");
        assert_eq!(parsed.to_string(), "26.10.20");
        assert_eq!(parsed, version("26.10.20"));
    }

    #[test]
    fn compares_components_numerically() {
        assert!(version("26.9.99") < version("26.10.0"));
        assert!(version("25.11.20") < version("26.1.0"));
        assert_eq!(version("26.10"), version("26.10.0"));
        assert_eq!(version("26.10.0.0"), version("26.10"));
        assert!(version("26.10.0") < version("26.10.1"));
    }

    #[test]
    fn rejects_malformed_versions() {
        for input in ["", "   ", "26..20", "26.x.1", "26.10.", ".26"] {
            assert!(input.parse::<Version>().is_err(), "{input:?} should fail");
        }
        assert_eq!(
            "26.x.1".parse::<Version>(),
            Err(ParseVersionError::InvalidComponent {
                index: 1,
                component: "x".to_owned(),
            })
        );
    }

    #[test]
    fn reads_the_pinned_version_file() {
        let temp = TempDir::new("pinned-version");
        let path = temp.path().join(PINNED_VERSION_FILE);
        fs::write(&path, "26.10.20\n").expect("write pinned version");
        assert_eq!(
            read_pinned_version(&path).expect("read pinned version"),
            version("26.10.20")
        );
    }

    #[test]
    fn reports_a_missing_pinned_version_file() {
        let temp = TempDir::new("missing-pinned-version");
        let error = read_pinned_version(temp.path().join(PINNED_VERSION_FILE))
            .expect_err("missing file should fail");
        assert!(matches!(error, PinnedVersionError::Io(_)));
    }
}
