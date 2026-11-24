//! Load Zalo Segoe + `zalo-font` at runtime from `assets/fonts` (or installed
//! XDG / system font directories).
//!
//! GPUI Linux `load_family` drops a face when swash `charmap().map('m') == 0`
//! (allowlist is only Segoe Fluent Icons). Swash prefers cmap **format 12**
//! over format 4. `zalo-font` is PUA-only (fmt12 groups 0xEA00…0xEE18), so an
//! in-memory fmt12 group maps Latin `m` → glyph 1. Format 4 is left alone:
//! its `idRangeOffset=4` glyph array would break PUA if a segment were
//! inserted in front.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use gpui::{div, prelude::*, px, App, IntoElement, ParentElement, Styled};

/// TTF name ID 1 of `Zalo-Segoe-UI*.ttf` Regular.
pub const FAMILY_UI: &str = "Zalo Segoe UI";
/// Separate family (the Semibold file is Regular of this family).
pub const FAMILY_SEMIBOLD: &str = "Zalo Segoe UI Semibold";
/// Separate family for Bold.
pub const FAMILY_BOLD: &str = "Zalo Segoe UI Bold";
/// Optical size for message bodies.
pub const FAMILY_TEXT: &str = "Zalo Segoe UI Text";
/// Icon face (`@font-face zalo-font`).
pub const FAMILY_ICON: &str = "zalo-font";
/// Font Awesome 6 Free (name id 16). Solid face, weight 900.
/// SIL OFL, reserved name "Font Awesome". See `assets/fa-solid-900-LICENSE.txt`.
pub const FAMILY_AWESOME: &str = "Font Awesome 6 Free";
/// `fa-solid fa-sun` (U+F185).
pub const FA_SUN: char = '\u{F185}';
/// `fa-solid fa-moon` (U+F186).
pub const FA_MOON: char = '\u{F186}';
/// `fa-solid fa-gear` (U+F013).
#[allow(dead_code)]
pub const FA_GEAR: char = '\u{F013}';
/// `fa-solid fa-globe` (U+F0AC).
#[allow(dead_code)]
pub const FA_GLOBE: char = '\u{F0AC}';
/// `fa-solid fa-trash` (U+F1F8).
#[allow(dead_code)]
pub const FA_TRASH: char = '\u{F1F8}';
/// `fa-solid fa-rotate` (U+F2F1).
#[allow(dead_code)]
pub const FA_ROTATE: char = '\u{F2F1}';
/// `fa-solid fa-xmark` (U+F00D).
#[allow(dead_code)]
pub const FA_XMARK: char = '\u{F00D}';
/// `fa-solid fa-plus` (U+F067).
#[allow(dead_code)]
pub const FA_PLUS: char = '\u{F067}';
/// `fa-solid fa-paper-plane` (U+F1D8). Composer send arrow.
pub const FA_PAPER_PLANE: char = '\u{F1D8}';
/// `fa-solid fa-folder-open` (U+F07C).
#[allow(dead_code)]
pub const FA_FOLDER_OPEN: char = '\u{F07C}';
/// `fa-solid fa-right-from-bracket` (U+F2F5).
#[allow(dead_code)]
pub const FA_RIGHT_FROM_BRACKET: char = '\u{F2F5}';
/// `fa-solid fa-cart-shopping` (U+F07A).
#[allow(dead_code)]
pub const FA_CART_SHOPPING: char = '\u{F07A}';
/// `fa-solid fa-crown` (U+F521).
#[allow(dead_code)]
pub const FA_CROWN: char = '\u{F521}';
/// `fa-solid fa-play` (U+F04B).
#[allow(dead_code)]
pub const FA_PLAY: char = '\u{F04B}';
/// `fa-solid fa-stop` (U+F04D).
#[allow(dead_code)]
pub const FA_STOP: char = '\u{F04D}';
/// `fa-solid fa-check` (U+F00C).
#[allow(dead_code)]
pub const FA_CHECK: char = '\u{F00C}';
/// `fa-solid fa-arrow-up-right-from-square` (U+F08E).
#[allow(dead_code)]
pub const FA_EXTERNAL_LINK: char = '\u{F08E}';
/// `fa-solid fa-star` (U+F005).
#[allow(dead_code)]
pub const FA_STAR: char = '\u{F005}';
/// `fa-solid fa-lightbulb` (U+F0EB).
#[allow(dead_code)]
pub const FA_LIGHTBULB: char = '\u{F0EB}';
/// `fa-solid fa-angles-down` (U+F103).
#[allow(dead_code)]
pub const FA_ANGLES_DOWN: char = '\u{F103}';
/// `fa-solid fa-chevron-down` (U+F078).
#[allow(dead_code)]
pub const FA_CHEVRON_DOWN: char = '\u{F078}';

/// Render a Font Awesome 6 Free Solid glyph (weight 900) with a Unicode fallback.
pub fn fa_icon(glyph: char, fallback: &'static str, size_px: f32, color: u32) -> impl IntoElement {
    let loaded = awesome_loaded();
    div()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size_px))
        .line_height(px(size_px))
        .text_color(gpui::rgb(color))
        .when(loaded, |el| {
            el.font_family(awesome_family())
                .font_weight(gpui::FontWeight::BLACK)
        })
        .child(if loaded {
            glyph.to_string()
        } else {
            fallback.to_owned()
        })
}

/// Vendored `fa-solid-900.ttf`. Not renamed: the OFL reserves "Font Awesome".
const AWESOME_TTF: &[u8] = include_bytes!("../assets/fa-solid-900.ttf");

const BUNDLED_FONTS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../assets/fonts"
);

static ICON_OK: AtomicBool = AtomicBool::new(false);
static AWESOME_OK: AtomicBool = AtomicBool::new(false);
static AWESOME_FAMILY: OnceLock<String> = OnceLock::new();
static REPORT: OnceLock<FontReport> = OnceLock::new();

/// Actual registered family name for Font Awesome 6 Solid on this OS.
/// Linux registers `"Font Awesome 6 Free"` (name ID 16) while Windows
/// DirectWrite registers `"Font Awesome 6 Free Solid"` (name ID 1).
#[must_use]
pub fn awesome_family() -> &'static str {
    AWESOME_FAMILY
        .get()
        .map(String::as_str)
        .unwrap_or(FAMILY_AWESOME)
}

/// Which faces were found on disk and handed to GPUI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontReport {
    /// Regular Segoe.
    pub regular: bool,
    /// Semibold Segoe.
    pub semibold: bool,
    /// Bold Segoe.
    pub bold: bool,
    /// Text optical size.
    pub text: bool,
    /// Icon font after the cmap patch.
    pub icon: bool,
    /// Font Awesome 6 Free Solid, for the theme sun and moon.
    pub awesome: bool,
}

/// True when rail/composer can paint `zalo-font` code points.
pub fn icon_loaded() -> bool {
    ICON_OK.load(Ordering::Relaxed)
}

/// True when the theme pill can paint Font Awesome sun / moon.
pub fn awesome_loaded() -> bool {
    AWESOME_OK.load(Ordering::Relaxed)
}

/// Last install report (empty before `install`).
pub fn report() -> FontReport {
    REPORT.get().copied().unwrap_or_default()
}

/// Role of a decoded Segoe file. Match the file name, do not sniff bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegoeRole {
    /// `SegoeuiPc` weight 400.
    Regular,
    /// Weight 500/600.
    Semibold,
    /// Weight 700+.
    Bold,
    /// `SegoeuiPcText`.
    Text,
    /// Light and other cuts.
    Other,
}

/// `Zalo-Segoe-UI.` has a dot so it must not swallow Semibold/Bold/Text.
pub fn segoe_role(name: &str) -> SegoeRole {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if file == "ZaloSegoeUI-Regular.ttf" || file.starts_with("Zalo-Segoe-UI.") {
        return SegoeRole::Regular;
    }
    if file == "ZaloSegoeUI-Semibold.ttf"
        || file == "ZaloSegoeUI-SemiBold.ttf"
        || file.starts_with("Zalo-Segoe-UI-Semibold.")
    {
        return SegoeRole::Semibold;
    }
    if file == "ZaloSegoeUI-Bold.ttf" || file.starts_with("Zalo-Segoe-UI-Bold.") {
        return SegoeRole::Bold;
    }
    if file.starts_with("Zalo-Segoe-UI-Text.") {
        return SegoeRole::Text;
    }
    SegoeRole::Other
}

/// Read font faces from user/system/bundled font folders and register them with GPUI.
pub fn install(cx: &App) -> FontReport {
    let mut blobs: Vec<Cow<'static, [u8]>> = Vec::new();
    let mut found = FontReport::default();
    let mut loaded = Vec::new();

    if let Some((name, bytes)) = read_segoe(SegoeRole::Regular) {
        blobs.push(Cow::Owned(bytes));
        found.regular = true;
        loaded.push(format!("regular={name}"));
    }
    if let Some((name, bytes)) = read_segoe(SegoeRole::Semibold) {
        blobs.push(Cow::Owned(bytes));
        found.semibold = true;
        loaded.push(format!("semibold={name}"));
    }
    if let Some((name, bytes)) = read_segoe(SegoeRole::Bold) {
        blobs.push(Cow::Owned(bytes));
        found.bold = true;
        loaded.push(format!("bold={name}"));
    }
    if let Some((name, bytes)) = read_segoe(SegoeRole::Text) {
        blobs.push(Cow::Owned(bytes));
        found.text = true;
        loaded.push(format!("text={name}"));
    }
    if let Some((name, bytes)) = read_icon_font() {
        match patch_letter_m(bytes) {
            Some(patched) => {
                blobs.push(Cow::Owned(patched));
                found.icon = true;
                loaded.push(format!("icon={name}"));
            }
            None => loaded.push(format!("icon={name}(cmap-skip)")),
        }
    }
    blobs.push(Cow::Borrowed(AWESOME_TTF));
    found.awesome = true;
    loaded.push("awesome=fa-solid-900.ttf".to_owned());

    if !blobs.is_empty() {
        if let Err(err) = cx.text_system().add_fonts(blobs) {
            eprintln!("zalo-gpui fonts: add_fonts failed: {err}");
        }
    }
    if found.icon {
        let names = cx.text_system().all_font_names();
        let zalo: Vec<_> = names
            .iter()
            .filter(|name| name.to_ascii_lowercase().contains("zalo"))
            .cloned()
            .collect();
        eprintln!("zalo-gpui font-names: {}", zalo.join(" "));
    }
    ICON_OK.store(found.icon, Ordering::Relaxed);
    AWESOME_OK.store(found.awesome, Ordering::Relaxed);
    if found.awesome {
        let names = cx.text_system().all_font_names();
        let awesome: Vec<_> = names
            .iter()
            .filter(|name| name.to_ascii_lowercase().contains("awesome"))
            .cloned()
            .collect();
        if let Some(picked) = awesome
            .iter()
            .find(|n| n.eq_ignore_ascii_case(FAMILY_AWESOME))
            .or_else(|| awesome.first())
        {
            let _ = AWESOME_FAMILY.set(picked.clone());
        }
        eprintln!("zalo-gpui font-awesome: {}", awesome.join(" "));
    }
    let _ = REPORT.set(found);
    eprintln!("zalo-gpui fonts: {}", loaded.join(" "));
    found
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data) = zalo_shell::data_home() {
        dirs.push(data.join("zalo-rust").join("fonts"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("fonts"));
            #[cfg(target_os = "macos")]
            dirs.push(parent.join("../Resources/zalo-rust/fonts"));
        }
    }
    dirs.push(PathBuf::from("/usr/share/zalo-rust/fonts"));
    dirs.push(PathBuf::from(BUNDLED_FONTS));
    dirs
}

/// Active runtime font paths (`Regular`, `Semibold`, `Bold`, `Text`, `zalo-font`).
pub(crate) fn runtime_font_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen_roles = Vec::new();
    for dir in font_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let role = segoe_role(name);
            if matches!(
                role,
                SegoeRole::Regular | SegoeRole::Semibold | SegoeRole::Bold | SegoeRole::Text
            ) && !seen_roles.contains(&role)
                && path.is_file()
            {
                seen_roles.push(role);
                out.push(path);
                continue;
            }
            if name.starts_with("zalo-font.")
                && name.ends_with(".ttf")
                && !out.iter().any(|p| p.file_name() == path.file_name())
                && path.is_file()
            {
                out.push(path);
            }
        }
    }
    out
}

fn read_segoe(role: SegoeRole) -> Option<(String, Vec<u8>)> {
    for dir in font_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if segoe_role(name) != role {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                return Some((name.to_owned(), bytes));
            }
        }
    }
    None
}

fn read_icon_font() -> Option<(String, Vec<u8>)> {
    for dir in font_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("zalo-font.") && name.ends_with(".ttf"))
            })
            .collect();
        paths.sort();
        if let Some(path) = paths.into_iter().next() {
            if let (Some(name), Ok(bytes)) = (
                path.file_name().and_then(|n| n.to_str()),
                std::fs::read(&path),
            ) {
                return Some((name.to_owned(), bytes));
            }
        }
    }
    None
}

/// Map Latin `m` to glyph 1 in cmap format 12 so GPUI keeps the PUA icon face.
fn patch_letter_m(font: Vec<u8>) -> Option<Vec<u8>> {
    if cmap_has_m(&font) {
        return Some(font);
    }
    let cmap = table_slice(&font, *b"cmap")?;
    let patched_cmap = patch_cmap_fmt12(cmap)?;
    splice_table(&font, *b"cmap", &patched_cmap)
}

fn cmap_has_m(font: &[u8]) -> bool {
    let Some(cmap) = table_slice(font, *b"cmap") else {
        return false;
    };
    cmap_fmt12_glyph(cmap, u32::from(b'm')).is_some()
}

fn cmap_records(cmap: &[u8]) -> Option<Vec<(u16, u16, usize)>> {
    if cmap.len() < 4 {
        return None;
    }
    let n = u16::from_be_bytes(cmap[2..4].try_into().ok()?) as usize;
    let mut records = Vec::with_capacity(n);
    for i in 0..n {
        let rec = 4 + i * 8;
        if rec + 8 > cmap.len() {
            return None;
        }
        let plat = u16::from_be_bytes(cmap[rec..rec + 2].try_into().ok()?);
        let enc = u16::from_be_bytes(cmap[rec + 2..rec + 4].try_into().ok()?);
        let offset = u32::from_be_bytes(cmap[rec + 4..rec + 8].try_into().ok()?) as usize;
        records.push((plat, enc, offset));
    }
    Some(records)
}

fn cmap_fmt12_glyph(cmap: &[u8], cp: u32) -> Option<u16> {
    let records = cmap_records(cmap)?;
    for (_, _, offset) in records {
        if offset + 2 > cmap.len() {
            continue;
        }
        let fmt = u16::from_be_bytes(cmap[offset..offset + 2].try_into().ok()?);
        if fmt == 12 {
            return fmt12_glyph(cmap.get(offset..)?, cp);
        }
    }
    None
}

fn fmt12_glyph(fmt12: &[u8], cp: u32) -> Option<u16> {
    if fmt12.len() < 16 {
        return None;
    }
    let format = u16::from_be_bytes(fmt12[0..2].try_into().ok()?);
    if format != 12 {
        return None;
    }
    let length = u32::from_be_bytes(fmt12[4..8].try_into().ok()?) as usize;
    let n_groups = u32::from_be_bytes(fmt12[12..16].try_into().ok()?) as usize;
    if length > fmt12.len() || 16 + n_groups * 12 > fmt12.len() {
        return None;
    }
    for i in 0..n_groups {
        let rec = 16 + i * 12;
        let start = u32::from_be_bytes(fmt12[rec..rec + 4].try_into().ok()?);
        let end = u32::from_be_bytes(fmt12[rec + 4..rec + 8].try_into().ok()?);
        let gid = u32::from_be_bytes(fmt12[rec + 8..rec + 12].try_into().ok()?);
        if start <= cp && cp <= end {
            return Some((cp - start + gid) as u16);
        }
    }
    None
}

#[cfg(test)]
fn fmt4_covers(fmt4: &[u8], cp: u16) -> bool {
    let Some((seg_count, end_code, start_code, _, _)) = parse_fmt4(fmt4) else {
        return false;
    };
    (0..seg_count).any(|i| start_code[i] <= cp && cp <= end_code[i] && cp != 0xffff)
}

#[cfg(test)]
fn parse_fmt4(fmt4: &[u8]) -> Option<(usize, Vec<u16>, Vec<u16>, Vec<u16>, Vec<u16>)> {
    if fmt4.len() < 16 {
        return None;
    }
    let format = u16::from_be_bytes(fmt4[0..2].try_into().ok()?);
    if format != 4 {
        return None;
    }
    let length = u16::from_be_bytes(fmt4[2..4].try_into().ok()?) as usize;
    if length > fmt4.len() {
        return None;
    }
    let seg_count = u16::from_be_bytes(fmt4[6..8].try_into().ok()?) as usize / 2;
    let mut pos = 14;
    let end_code = read_u16s(fmt4, &mut pos, seg_count)?;
    pos += 2; // reservedPad
    let start_code = read_u16s(fmt4, &mut pos, seg_count)?;
    let id_delta = read_u16s(fmt4, &mut pos, seg_count)?;
    let id_range = read_u16s(fmt4, &mut pos, seg_count)?;
    Some((seg_count, end_code, start_code, id_delta, id_range))
}

#[cfg(test)]
fn read_u16s(data: &[u8], pos: &mut usize, n: usize) -> Option<Vec<u16>> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        if *pos + 2 > data.len() {
            return None;
        }
        out.push(u16::from_be_bytes(data[*pos..*pos + 2].try_into().ok()?));
        *pos += 2;
    }
    Some(out)
}

/// Insert `{start=end=U+006D, startGlyphID=1}` into every unique format-12
/// subtable. Groups stay sorted. Other cmap records that share the offset
/// keep pointing at the same blob.
fn patch_cmap_fmt12(cmap: &[u8]) -> Option<Vec<u8>> {
    let records = cmap_records(cmap)?;
    let mut fmt12_offs: Vec<usize> = records
        .iter()
        .filter_map(|(_, _, offset)| {
            cmap.get(*offset..offset + 2).and_then(|bytes| {
                u16::from_be_bytes(bytes.try_into().ok()?)
                    .eq(&12)
                    .then_some(*offset)
            })
        })
        .collect();
    fmt12_offs.sort_unstable();
    fmt12_offs.dedup();
    if fmt12_offs.is_empty() {
        return None;
    }
    let mut body = cmap.to_vec();
    for offset in fmt12_offs.into_iter().rev() {
        body = insert_fmt12_m(body, offset)?;
    }
    Some(body)
}

fn insert_fmt12_m(cmap: Vec<u8>, off: usize) -> Option<Vec<u8>> {
    if off + 16 > cmap.len() {
        return None;
    }
    let fmt = u16::from_be_bytes(cmap[off..off + 2].try_into().ok()?);
    if fmt != 12 {
        return None;
    }
    let old_len = u32::from_be_bytes(cmap[off + 4..off + 8].try_into().ok()?) as usize;
    let n_groups = u32::from_be_bytes(cmap[off + 12..off + 16].try_into().ok()?) as usize;
    if old_len < 16 + n_groups * 12 || off + old_len > cmap.len() {
        return None;
    }
    if fmt12_glyph(&cmap[off..off + old_len], u32::from(b'm')).is_some() {
        return Some(cmap);
    }
    let m = u32::from(b'm');
    let gid = 1u32;
    let groups_at = off + 16;
    let mut insert_at = n_groups;
    for i in 0..n_groups {
        let start = u32::from_be_bytes(
            cmap[groups_at + i * 12..groups_at + i * 12 + 4]
                .try_into()
                .ok()?,
        );
        if start > m {
            insert_at = i;
            break;
        }
    }
    let new_len = old_len + 12;
    let new_groups = n_groups + 1;
    let insert_byte = groups_at + insert_at * 12;
    let mut out = Vec::with_capacity(cmap.len() + 12);
    out.extend_from_slice(&cmap[..off]);
    out.extend_from_slice(&12u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(new_len as u32).to_be_bytes());
    out.extend_from_slice(&cmap[off + 8..off + 12]); // language
    out.extend_from_slice(&(new_groups as u32).to_be_bytes());
    out.extend_from_slice(&cmap[off + 16..insert_byte]);
    out.extend_from_slice(&m.to_be_bytes());
    out.extend_from_slice(&m.to_be_bytes());
    out.extend_from_slice(&gid.to_be_bytes());
    out.extend_from_slice(&cmap[insert_byte..off + old_len]);
    out.extend_from_slice(&cmap[off + old_len..]);
    let n = u16::from_be_bytes(out[2..4].try_into().ok()?) as usize;
    for i in 0..n {
        let rec = 4 + i * 8;
        let offset = u32::from_be_bytes(out[rec + 4..rec + 8].try_into().ok()?) as usize;
        if offset > off {
            let next = (offset + 12) as u32;
            out[rec + 4..rec + 8].copy_from_slice(&next.to_be_bytes());
        }
    }
    Some(out)
}

fn table_slice(font: &[u8], tag: [u8; 4]) -> Option<&[u8]> {
    let (offset, length) = table_loc(font, tag)?;
    font.get(offset..offset + length)
}

fn table_loc(font: &[u8], tag: [u8; 4]) -> Option<(usize, usize)> {
    if font.len() < 12 {
        return None;
    }
    let n = u16::from_be_bytes(font[4..6].try_into().ok()?) as usize;
    for i in 0..n {
        let rec = 12 + i * 16;
        if font.get(rec..rec + 4) != Some(&tag[..]) {
            continue;
        }
        let offset = u32::from_be_bytes(font[rec + 8..rec + 12].try_into().ok()?) as usize;
        let length = u32::from_be_bytes(font[rec + 12..rec + 16].try_into().ok()?) as usize;
        return Some((offset, length));
    }
    None
}

fn splice_table(font: &[u8], tag: [u8; 4], new_data: &[u8]) -> Option<Vec<u8>> {
    let (offset, length) = table_loc(font, tag)?;
    let delta = new_data.len() as i32 - length as i32;
    let mut out = Vec::with_capacity(font.len() + delta.max(0) as usize);
    out.extend_from_slice(&font[..offset]);
    out.extend_from_slice(new_data);
    let old_end = offset + length;
    let pad = (4 - new_data.len() % 4) % 4;
    let old_pad = (4 - length % 4) % 4;
    out.extend(std::iter::repeat_n(0u8, pad));
    let rest_start = old_end + old_pad;
    if rest_start <= font.len() {
        out.extend_from_slice(&font[rest_start..]);
    }
    let shift = (new_data.len() + pad) as i32 - (length + old_pad) as i32;
    let n = u16::from_be_bytes(out[4..6].try_into().ok()?) as usize;
    for i in 0..n {
        let rec = 12 + i * 16;
        let table_off = u32::from_be_bytes(out[rec + 8..rec + 12].try_into().ok()?);
        if table_off as usize > offset {
            let next = (table_off as i32 + shift) as u32;
            out[rec + 8..rec + 12].copy_from_slice(&next.to_be_bytes());
        }
        if out[rec..rec + 4] == tag {
            out[rec + 12..rec + 16].copy_from_slice(&(new_data.len() as u32).to_be_bytes());
        }
    }
    Some(out)
}

/// `zalo-font` PUA from `.fa-*:after` (CSS 26.10.10), same as egui `icon_char`.
pub mod ic {
    /// `Message_24_Line` / filled when selected.
    pub const CHAT: char = '\u{EC5A}';
    /// `Message_24_Filled`.
    pub const CHAT_ON: char = '\u{EC59}';
    /// `Contact_24_Line`.
    pub const CONTACTS: char = '\u{EABE}';
    /// `Contact_24_Filled`.
    pub const CONTACTS_ON: char = '\u{EABD}';
    /// `Setting_24_Line`.
    pub const GEAR: char = '\u{ED24}';
    /// `Setting_24_Filled`.
    pub const GEAR_ON: char = '\u{ED23}';
    /// `Biztool_24_Line`.
    pub const BRIEFCASE: char = '\u{EA2D}';
    /// `Biztool_24_Filled`.
    pub const BRIEFCASE_ON: char = '\u{EA2C}';
    /// `Screenshot_28`.
    pub const CAPTURE: char = '\u{ED11}';
    /// `MyDocs_24_Line`.
    pub const DOCS: char = '\u{EC6E}';
    /// `MyDocs_24_Filled`.
    pub const DOCS_ON: char = '\u{EC6D}';
    /// `MyDocument_Ava`. The disc and the folder share one fill, so a white
    /// glyph on a blue circle is a blank disc. Cloud avatars use [`DOCS_ON`].
    #[allow(dead_code)]
    pub const MY_DOC_AVA: char = '\u{EC6F}';
    /// `Extend_24_Line` — trailing glyph on "Upgrade Account".
    pub const EXTEND: char = '\u{EB10}';
    /// `edit-name` — pencil beside the account name.
    pub const EDIT_NAME: char = '\u{EAF2}';
    /// `Edit_2_24_Line` — pencil on a pending-photo thumb.
    pub const EDIT_PHOTO: char = '\u{EAEF}';
    /// `Trash_24_Line` — remove one pending photo.
    pub const TRASH_LINE: char = '\u{EDCF}';
    /// `zCloud_24_Line`.
    pub const CLOUD: char = '\u{EE13}';
    /// `zCloud_24_Filled`.
    pub const CLOUD_ON: char = '\u{EE12}';
    /// `Group_Create_24`.
    pub const GROUP_ADD: char = '\u{EB45}';
    /// `Call_24`.
    pub const PHONE: char = '\u{EA41}';
    /// `FriendList_24`.
    pub const FRIEND_LIST: char = '\u{EB3C}';
    /// `GroupList_24`.
    pub const GROUP_LIST: char = '\u{EB4A}';
    /// `FriendRequest_24`.
    pub const FRIEND_REQ: char = '\u{EB3D}';
    /// `GroupRequest_24`.
    pub const GROUP_REQ: char = '\u{EB50}';
    /// `Setting_24` (Settings menu icon, distinct from `GEAR` on the rail).
    pub const SETTING: char = '\u{ED22}';
    /// `Security_24`.
    pub const SECURITY: char = '\u{ED1D}';
    /// `Privacy_24`.
    pub const PRIVACY: char = '\u{ECDB}';
    /// `Storage_24`.
    pub const STORAGE: char = '\u{ED7D}';
    /// `Theme_24`.
    pub const THEME: char = '\u{EDAC}';
    /// `Notification_24_Line`.
    pub const NOTIF_ON: char = '\u{EC76}';
    /// `MessageSetting_24`.
    pub const MESSAGES: char = '\u{EC5D}';
    /// `Utility_24`.
    pub const UTILITY: char = '\u{EDEB}';
    /// `User_24`.
    pub const USER: char = '\u{EDE9}';
    /// `fa-icon-emoji`.
    #[allow(dead_code)]
    pub const EMOJI: char = '\u{EAF9}';
    // ── Live DOM glyphs (Zalo PC 26.10.10, font 6835d6, CDP 11/11/2026). ──
    /// `fa-search` — search bar icon (`.group-search__icon`).
    pub const SEARCH_FIELD: char = '\u{ED1C}';
    /// `fa-outline-add-new-contact-2` — "Add Friend" next to search.
    pub const ADD_CONTACT: char = '\u{EC87}';
    /// `fa-More_24_Line` — row hover "…" / composer "More Options".
    pub const CONV_MORE: char = '\u{EC65}';
    /// `fa-solid-more` — tab bar «…».
    pub const SOLID_MORE: char = '\u{ED56}';
    /// `fa-icon-solid-down` — "Labels" chevron.
    pub const SOLID_DOWN: char = '\u{EBFF}';
    /// `fa-icon-outline-picture` — preview "Photo".
    pub const PICTURE: char = '\u{EBDD}';
    /// `fa-Pin_24_Filled` — pinned row marker.
    pub const PIN: char = '\u{ECCA}';
    /// `fa-Extend_24_Line` — conversation row "Open in Separate Window".
    pub const CONV_EXTEND: char = '\u{EB10}';
    /// `fa-icon-solid-muted` — muted conversation marker.
    pub const SOLID_MUTED: char = '\u{EC0F}';
    /// `fa-Group_Create_24_Line` — header add member (group).
    pub const HDR_ADD_MEMBER: char = '\u{EB45}';
    /// `fa-outline-call` — header voice call (1-1).
    pub const HDR_CALL: char = '\u{EC8F}';
    /// `fa-icon-outline-video` — header video call.
    pub const HDR_VIDEO: char = '\u{EBF0}';
    /// `fa-outline-search` — header search messages.
    pub const HDR_SEARCH: char = '\u{ECAD}';
    /// `fa-outline-right-bar` — header conversation info.
    pub const HDR_RIGHT_BAR: char = '\u{ECAB}';
    /// `fa-outline-contact` — group subtitle members.
    pub const HDR_MEMBERS: char = '\u{EC91}';
    /// `fa-outline-tag` — subtitle label tag.
    pub const HDR_TAG: char = '\u{ECB1}';
    /// `fa-Sticker_24_Line` — composer sticker.
    pub const TB_STICKER: char = '\u{ED75}';
    /// `fa-Photo_24_Line` — composer photo.
    pub const TB_PHOTO: char = '\u{ECC5}';
    /// `Plus_24_Line` — add another photo in the attach tray.
    pub const PLUS_24: char = '\u{ECD6}';
    /// `fa-Attach_24_Line` — composer file.
    pub const TB_ATTACH: char = '\u{EA22}';
    /// `fa-Namecard_24_Line` — composer business card.
    pub const TB_NAMECARD: char = '\u{EC72}';
    /// `fa-Screenshot-Z_24_Line` — composer screenshot that keeps the Zalo window.
    pub const TB_SHOT: char = '\u{ED13}';
    /// `Screenshot_24` — composer screenshot that hides the Zalo window.
    pub const TB_SHOT_PLAIN: char = '\u{ED10}';
    /// `fa-Chevron_Down_24_Filled` — composer screenshot mode.
    pub const TB_CHEVRON: char = '\u{EA8B}';
    /// `Chevron_Double_Down_32_Line` — official Zalo scroll-to-bottom double chevron.
    pub const CHEVRON_DOUBLE_DOWN: char = '\u{EA89}';
    /// `chevron-left` — official Zalo horizontal scroll left arrow.
    pub const CHEVRON_LEFT: char = '\u{EA94}';
    /// `chevron-right` — official Zalo horizontal scroll right arrow.
    pub const CHEVRON_RIGHT: char = '\u{EA95}';
    /// `Close_24_Line` — official Zalo crisp line close icon.
    pub const CLOSE_LINE: char = '\u{EA9F}';
    /// `textbox-icon-clear` — official Zalo search/input clear circle icon.
    pub const TEXTBOX_CLEAR: char = '\u{EDA7}';
    /// `fa-TextFormat_24_Line` — composer text format.
    pub const TB_FORMAT: char = '\u{EDAB}';
    /// `fa-Quick-msg` — composer quick message.
    pub const TB_QUICK: char = '\u{ECE9}';
    /// `fa-icon-outline-bank-card` — composer bank card.
    pub const TB_BANK: char = '\u{EBB9}';
    /// `fa-Emoji_24_Line` — composer emoji (right).
    pub const TB_EMOJI: char = '\u{EAF9}';
    /// `fa-Mic_24_Line` — composer voice message.
    pub const TB_MIC: char = '\u{EC5F}';
    /// `fa-Stop_24_Filled` — stop the voice recording.
    pub const TB_STOP: char = '\u{ED76}';
    /// `fa-Quote_24_Filled` — bubble hover "Reply".
    pub const MSG_QUOTE: char = '\u{ECEC}';
    /// `fa-Share_24_Filled` — bubble hover "Share".
    pub const MSG_SHARE: char = '\u{ED29}';
    /// `fa-MoreH_24_Line` — bubble hover «…».
    pub const MSG_MORE: char = '\u{EC66}';
    /// `fa-chat_bubble_icon_call_outgoing` — preview outgoing call.
    pub const CALL_OUT: char = '\u{EA57}';
    /// `fa-chat_bubble_icon_call_incoming` — preview incoming call.
    pub const CALL_IN: char = '\u{EA56}';
    /// `fa-chat_bubble_icon_misscall` — preview missed call.
    pub const CALL_MISS: char = '\u{EA58}';
    /// `fa-chat-call-menu-video-16` — video call camcorder icon for bubbles & previews.
    pub const CALL_VIDEO: char = '\u{EA5A}';
    /// `fa-File_24_Line` — preview file.
    pub const FILE: char = '\u{EB1B}';
    /// `fa-icon-outline-sticker` — preview sticker.
    pub const STICKER_PREVIEW: char = '\u{EBE8}';
}

/// Glyph string: PUA when the icon face loaded, else a Latin stand-in.
pub fn glyph(code: char, fallback: &'static str) -> String {
    if icon_loaded() {
        code.to_string()
    } else {
        fallback.to_owned()
    }
}

/// Square icon cell: `line-height` equals `size` so GPUI φ-leading cannot
/// shove the PUA glyph off-center (rail icons looked shifted right). No
/// `overflow_hidden` because `zalo-font` glyphs overflow `0..1000 UPM` (`ymax`
/// reaches `1017`), which otherwise clips the top of tall icons like search.
pub fn icon_el(code: char, fallback: &'static str, size: f32) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .text_size(px(size))
        .line_height(px(size))
        .when(icon_loaded(), |el| el.font_family(FAMILY_ICON))
        .child(glyph(code, fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segoe_role_matches_hashed_names() {
        assert_eq!(
            segoe_role("Zalo-Segoe-UI.dc1a7d868a0b3a0e2daa8b36567e9ea9.ttf"),
            SegoeRole::Regular
        );
        assert_eq!(segoe_role("ZaloSegoeUI-Regular.ttf"), SegoeRole::Regular);
        assert_eq!(
            segoe_role("Zalo-Segoe-UI-Semibold.8de26695631b05cf79e17378f6d1f134.ttf"),
            SegoeRole::Semibold
        );
        assert_eq!(segoe_role("ZaloSegoeUI-SemiBold.ttf"), SegoeRole::Semibold);
        assert_eq!(
            segoe_role("Zalo-Segoe-UI-Bold.18b859523696ff3330adac79b99ef554.ttf"),
            SegoeRole::Bold
        );
        assert_eq!(
            segoe_role("Zalo-Segoe-UI-Text.41364bc2c9bcbffced63c4bad472bfa9.ttf"),
            SegoeRole::Text
        );
        assert_eq!(
            segoe_role("Zalo-Segoe-UI-Light.59fe5c1f89938a17b393b5ae607255ee.ttf"),
            SegoeRole::Other
        );
    }

    #[test]
    fn decoded_dir_has_required_faces() {
        assert!(read_segoe(SegoeRole::Regular).is_some());
        assert!(read_segoe(SegoeRole::Semibold).is_some());
        assert!(read_segoe(SegoeRole::Bold).is_some());
        assert!(read_segoe(SegoeRole::Text).is_some());
        assert!(read_icon_font().is_some());
        assert_eq!(FAMILY_UI, "Zalo Segoe UI");
        assert_eq!(FAMILY_SEMIBOLD, "Zalo Segoe UI Semibold");
        assert_eq!(FAMILY_BOLD, "Zalo Segoe UI Bold");
        assert_eq!(FAMILY_TEXT, "Zalo Segoe UI Text");
        assert_eq!(FAMILY_ICON, "zalo-font");
        assert_eq!(FAMILY_AWESOME, "Font Awesome 6 Free");
        assert_eq!(report(), FontReport::default());
    }

    #[test]
    fn awesome_solid_maps_sun_and_moon() {
        let cmap = table_slice(AWESOME_TTF, *b"cmap").expect("cmap");
        assert!(
            cmap_fmt12_glyph(cmap, u32::from(b'm')).is_some(),
            "GPUI keeps faces that map m"
        );
        assert_eq!(cmap_fmt12_glyph(cmap, u32::from(FA_SUN)), Some(676));
        assert_eq!(cmap_fmt12_glyph(cmap, u32::from(FA_MOON)), Some(677));
    }

    #[test]
    fn icon_font_cmap_patch_adds_m() {
        let (_, bytes) = read_icon_font().expect("zalo-font ttf");
        let stock = table_slice(&bytes, *b"cmap").expect("stock cmap");
        assert!(
            cmap_fmt12_glyph(stock, u32::from(b'm')).is_none(),
            "stock icon font is PUA-only"
        );
        let chat_gid = cmap_fmt12_glyph(stock, u32::from(ic::CHAT as u32)).expect("CHAT PUA");
        let search_gid =
            cmap_fmt12_glyph(stock, u32::from(ic::SEARCH_FIELD as u32)).expect("SEARCH_FIELD PUA");
        let close_gid = cmap_fmt12_glyph(stock, u32::from(ic::PIN as u32)).expect("PIN PUA");
        assert_eq!(chat_gid, 591, "0xEC5A singleton gid in 26.10.20 zalo-font");

        let patched = patch_letter_m(bytes).expect("patch");
        let cmap = table_slice(&patched, *b"cmap").expect("patched cmap");
        assert_eq!(cmap_fmt12_glyph(cmap, u32::from(b'm')), Some(1));
        assert_eq!(
            cmap_fmt12_glyph(cmap, u32::from(ic::CHAT as u32)),
            Some(chat_gid)
        );
        assert_eq!(
            cmap_fmt12_glyph(cmap, u32::from(ic::SEARCH_FIELD as u32)),
            Some(search_gid)
        );
        assert_eq!(
            cmap_fmt12_glyph(cmap, u32::from(ic::PIN as u32)),
            Some(close_gid)
        );
        assert!(cmap_has_m(&patched));

        // Format 4 stays PUA-only; inserting a segment would break idRangeOffset=4.
        let fmt4_off = cmap_records(cmap)
            .expect("records")
            .into_iter()
            .find_map(|(_, _, off)| {
                cmap.get(off..off + 2)
                    .and_then(|b| u16::from_be_bytes(b.try_into().ok()?).eq(&4).then_some(off))
            })
            .expect("fmt4");
        assert!(
            !fmt4_covers(&cmap[fmt4_off..], u16::from(b'm')),
            "fmt4 must not grow"
        );
        assert!(fmt4_covers(&cmap[fmt4_off..], ic::CHAT as u32 as u16));
        assert_eq!(ic::CHAT, '\u{EC5A}');
        assert_eq!(ic::GEAR, '\u{ED24}');
        assert_eq!(ic::USER, '\u{EDE9}');
        assert_eq!(ic::EMOJI, '\u{EAF9}');
        assert_eq!(ic::SETTING, '\u{ED22}');
        assert_eq!(ic::PIN, '\u{ECCA}');
        assert!(table_loc(&patched, *b"glyf").is_some());
        assert!(table_loc(&patched, *b"loca").is_some());
    }
}
