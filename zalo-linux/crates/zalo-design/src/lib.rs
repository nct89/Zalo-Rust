//! Zalo design reference for the Rust rewrite.
//!
//! Generated from the desktop UI assets.
//! It contains no Zalo code: only design tokens, Vietnamese and English UI strings,
//! and measured layout constants.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod layout;
pub mod css_layout;
pub mod strings_en;
pub mod strings_vi;
pub mod tokens;

pub use layout::*;
pub use strings_en::{tr_en, EN, EN_COUNT};
pub use strings_vi::{tr, VI, VI_COUNT};
pub use tokens::{dark, light, Theme, DARK_THEME, LIGHT_THEME};
