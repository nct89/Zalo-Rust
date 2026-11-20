//! Shared foundations for the Rust components of Zalo for Linux.
//!
//! - [`paths`] resolves XDG directories and the user's download folder.
//! - [`version`] parses, displays and compares Zalo release versions.
//!
//! This crate does not talk to Zalo, Electron or the network, and the
//! application does not link it yet. It is the base the new Rust work builds
//! on, not a finished feature.

#![forbid(unsafe_code)]

pub mod paths;
pub mod version;

pub use paths::{PathError, XdgDirs};
pub use version::{
    read_pinned_version, ParseVersionError, PinnedVersionError, Version, PINNED_VERSION_FILE,
};

#[cfg(test)]
mod test_support;
