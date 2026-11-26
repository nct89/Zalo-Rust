//! Zalo PC emoji picker tiles.
//!
//! Electron (`compact2760.js`) walks `emoji-pos.json` in insertion order,
//! skips indices 87–145 (uppercase duplicates) and a few hidden keys, then
//! splits the remaining list into nine categories (Zalo, People, …).
//! Both classic emoticons (`:)`, `/-heart`, …) and Unicode emojis live on the
//! single 51 × 41 sprite sheet (`emoji.1e7786c93c8a0c1773f165e2de2fd129.png`,
//! 64×64 px per cell) and are cropped lazily in memory into GPU-ready BGRA
//! [`gpui::RenderImage`] tiles with zero per-tile disk files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::RenderImage;
use image::Frame;

/// One tile in the emoji picker.
#[derive(Clone, Debug)]
pub struct EmojiItem {
    /// Text inserted into the composer (classic shortcode or Unicode).
    pub insert: String,
    /// Tooltip (the original map key).
    pub tip: String,
    /// Zero-based column on the 51 × 41 sprite sheet.
    pub col: u16,
    /// Zero-based row on the 51 × 41 sprite sheet.
    pub row: u16,
    /// Category index into [`GROUPS`] (0 = Zalo classic).
    pub group: u8,
}

/// Chip / rail labels under the EMOJI tab, same order as `compact2760.js`.
pub const GROUPS: [&str; 9] = [
    "Zalo",
    "Con người",
    "Tự nhiên",
    "Đồ ăn",
    "Hoạt động",
    "Du lịch",
    "Đồ vật",
    "Biểu tượng",
    "Quốc gia",
];

/// Start index of each category in the filtered Electron list.
const GROUP_POS: [usize; 9] = [0, 86, 290, 437, 504, 561, 676, 854, 1124];

const HIDDEN: &[&str] = &[":-|", ":+", ";8", "/-jj", "/-JJ"];

const DEV_EMOJI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/emoji");
/// Single 51 × 41 sprite sheet PNG containing every classic and Unicode emoji.
pub(crate) const SPRITE_FILE: &str = "emoji.1e7786c93c8a0c1773f165e2de2fd129.png";
/// Runtime files required in the `emoji/` asset directory.
pub(crate) const RUNTIME_FILES: [&str; 3] = ["emoji-pos.json", "emoji-rc.json", SPRITE_FILE];

/// Sprite is 51 × 41 cells of 64 px (CSS `background-size: 5100% 4100%`).
const SPRITE_COLS: u32 = 51;
const SPRITE_ROWS: u32 = 41;

/// Root of the emoji asset pack (`emoji-pos.json`, `emoji-rc.json`, sprite sheet).
/// Searches runtime install paths (Linux/macOS/Windows) before falling back to the dev tree.
#[must_use]
pub fn emoji_dir() -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(custom) = std::env::var("ZALO_EMOJI_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed));
        }
    }
    if let Some(data) = zalo_shell::data_home() {
        candidates.push(data.join("zalo-rust").join("emoji"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("emoji"));
            candidates.push(parent.join("assets").join("emoji"));
            candidates.push(parent.join("../share/zalo-rust/emoji"));
            #[cfg(target_os = "macos")]
            candidates.push(parent.join("../Resources/zalo-rust/emoji"));
        }
    }
    candidates.push(PathBuf::from("/app/share/zalo-rust/emoji"));
    candidates.push(PathBuf::from("/usr/share/zalo-rust/emoji"));
    candidates.push(PathBuf::from("/usr/local/share/zalo-rust/emoji"));
    let dev = PathBuf::from(DEV_EMOJI_DIR);
    candidates.push(dev.clone());
    candidates
        .into_iter()
        .find(|dir| dir.join("emoji-pos.json").is_file() && dir.join(SPRITE_FILE).is_file())
        .unwrap_or(dev)
}

/// Active runtime emoji files (the 3-file sprite pack) to copy during `--install`.
#[must_use]
pub(crate) fn runtime_emoji_paths() -> Vec<PathBuf> {
    let dir = emoji_dir();
    RUNTIME_FILES
        .into_iter()
        .map(|name| dir.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// Lazy catalogue matching Zalo PC's filtered `emojiObj`.
#[must_use]
pub fn catalog() -> &'static [EmojiItem] {
    static CATALOG: OnceLock<Vec<EmojiItem>> = OnceLock::new();
    CATALOG.get_or_init(load_catalog).as_slice()
}

/// Tiles belonging to `group` (`GROUPS` index).
#[must_use]
pub fn group_items(group: u8) -> impl Iterator<Item = &'static EmojiItem> {
    catalog().iter().filter(move |item| item.group == group)
}

/// Stable element/cache key for `item`.
#[must_use]
pub fn thumb_key(item: &EmojiItem) -> String {
    format!("emoji-{}-{}", item.col, item.row)
}

/// Lazily crops a 64 × 64 px BGRA [`RenderImage`] tile from the single in-memory
/// emoji sprite sheet and caches it by `(col, row)`.
#[must_use]
pub fn tile_image(item: &EmojiItem) -> Option<Arc<RenderImage>> {
    static TILES: OnceLock<Mutex<HashMap<(u16, u16), Arc<RenderImage>>>> = OnceLock::new();
    let cache = TILES.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (item.col, item.row);
    if let Some(hit) = cache.lock().ok().and_then(|m| m.get(&key).cloned()) {
        return Some(hit);
    }

    let sprite = sprite_bgra()?;
    let cell_w = sprite.width() / SPRITE_COLS;
    let cell_h = sprite.height() / SPRITE_ROWS;
    if cell_w == 0 || cell_h == 0 {
        return None;
    }
    let x = u32::from(item.col).saturating_mul(cell_w);
    let y = u32::from(item.row).saturating_mul(cell_h);
    if x + cell_w > sprite.width() || y + cell_h > sprite.height() {
        return None;
    }
    let sub = image::imageops::crop_imm(sprite, x, y, cell_w, cell_h).to_image();
    let image = Arc::new(RenderImage::new([Frame::new(sub)]));
    if let Ok(mut m) = cache.lock() {
        m.insert(key, Arc::clone(&image));
    }
    Some(image)
}

fn sprite_bgra() -> Option<&'static image::RgbaImage> {
    static SPRITE: OnceLock<Option<image::RgbaImage>> = OnceLock::new();
    SPRITE
        .get_or_init(|| {
            let path = emoji_dir().join(SPRITE_FILE);
            let img = image::open(path).ok()?.into_rgba8();
            Some(crate::media::rgba_to_bgra(img))
        })
        .as_ref()
}

/// True when `text` contains a classic shortcode or a catalogued Unicode emoji.
#[must_use]
#[cfg(test)]
pub fn contains_pictogram(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let first = classic_first_bytes();
    let has_trigger = text.bytes().any(|b| b >= 0x80 || first[b as usize]);
    if !has_trigger {
        return false;
    }
    let keys = classic_keys();
    let by_insert = by_insert();
    let mut rest = text;
    while !rest.is_empty() {
        let b = rest.as_bytes()[0];
        if b < 0x80 && first[b as usize] {
            if keys
                .iter()
                .any(|key| key.as_bytes()[0] == b && rest.starts_with(*key) && by_insert.contains_key(*key))
            {
                return true;
            }
        }
        let ch = rest.chars().next().unwrap();
        let ch_len = ch.len_utf8();
        if !ch.is_ascii() && by_insert.contains_key(&rest[..ch_len]) {
            return true;
        }
        rest = &rest[ch_len..];
    }
    false
}

/// One run of plain text or an emoji tile, in display order.
#[derive(Clone, Debug)]
pub enum PictogramPart {
    /// UTF-8 slice of the original string (owned so callers can drop `text`).
    Text(String),
    /// Catalog tile to paint inline.
    Tile(&'static EmojiItem),
}

/// Splits `text` into plain runs and catalogued pictograms (classic then Unicode).
#[must_use]
pub fn split_pictograms(text: &str) -> Vec<PictogramPart> {
    if text.is_empty() {
        return Vec::new();
    }
    let first = classic_first_bytes();
    if !text.bytes().any(|b| b >= 0x80 || first[b as usize]) {
        return vec![PictogramPart::Text(text.to_owned())];
    }
    let keys = classic_keys();
    let by_insert = by_insert();
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let b = rest.as_bytes()[0];
        let hit = if b < 0x80 && first[b as usize] {
            keys.iter()
                .find(|key| key.as_bytes()[0] == b && rest.starts_with(*key))
                .and_then(|key| by_insert.get(*key).copied().map(|item| (*key, item)))
        } else {
            None
        };
        if let Some((key, item)) = hit {
            if !buf.is_empty() {
                out.push(PictogramPart::Text(std::mem::take(&mut buf)));
            }
            out.push(PictogramPart::Tile(item));
            rest = &rest[key.len()..];
            continue;
        }
        let ch = rest.chars().next().unwrap();
        let ch_len = ch.len_utf8();
        if !ch.is_ascii() {
            if let Some(item) = by_insert.get(rest.get(..ch_len).unwrap_or("")).copied() {
                if !buf.is_empty() {
                    out.push(PictogramPart::Text(std::mem::take(&mut buf)));
                }
                out.push(PictogramPart::Tile(item));
                rest = &rest[ch_len..];
                continue;
            }
        }
        buf.push(ch);
        rest = &rest[ch_len..];
    }
    if !buf.is_empty() {
        out.push(PictogramPart::Text(buf));
    }
    out
}

fn classic_first_bytes() -> &'static [bool; 128] {
    static FIRST: OnceLock<[bool; 128]> = OnceLock::new();
    FIRST.get_or_init(|| {
        let mut table = [false; 128];
        for key in classic_keys() {
            if let Some(&b) = key.as_bytes().first() {
                if (b as usize) < 128 {
                    table[b as usize] = true;
                }
            }
        }
        table
    })
}

fn classic_keys() -> &'static [&'static str] {
    static KEYS: OnceLock<Vec<&'static str>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut keys: Vec<&'static str> = catalog()
            .iter()
            .filter(|item| item.group == 0)
            .map(|item| item.insert.as_str())
            .collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        keys
    })
    .as_slice()
}

fn by_insert() -> &'static HashMap<&'static str, &'static EmojiItem> {
    static MAP: OnceLock<HashMap<&'static str, &'static EmojiItem>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for item in catalog() {
            map.entry(item.insert.as_str()).or_insert(item);
            if item.tip != item.insert {
                map.entry(item.tip.as_str()).or_insert(item);
            }
        }
        // Ensure standard Unicode 👍 / ❤️ map to Zalo's full-color sprite tiles
        // instead of falling back to monochrome Linux text glyphs.
        if let Some(&strong) = map.get("/-strong") {
            map.entry("👍").or_insert(strong);
            map.entry("👍\u{FE0F}").or_insert(strong);
        }
        if let Some(&heart) = map.get("/-heart") {
            map.entry("❤️").or_insert(heart);
            map.entry("❤").or_insert(heart);
        }
        map
    })
}

fn load_catalog() -> Vec<EmojiItem> {
    let dir = emoji_dir();
    if !dir.join(SPRITE_FILE).is_file() {
        return Vec::new();
    }
    let pos_map = load_json_object(&dir.join("emoji-pos.json"));
    let rc_map = load_json_object(&dir.join("emoji-rc.json"));
    if pos_map.is_empty() {
        return Vec::new();
    }
    let mut cell_to_uni: HashMap<(u16, u16), String> = HashMap::new();
    for (key, value) in &rc_map {
        if key.is_ascii() {
            continue;
        }
        if let Some(cell) = parse_cell(value.as_str().unwrap_or("")) {
            cell_to_uni.entry(cell).or_insert_with(|| key.clone());
        }
    }
    let mut out = Vec::new();
    let mut idx = 0usize;
    for (key, pos_val) in &pos_map {
        idx += 1;
        if (87..=145).contains(&idx) || HIDDEN.contains(&key.as_str()) {
            continue;
        }
        if idx > 1440 {
            break;
        }
        let Some((x_pct, y_pct)) = pos_val.as_str().and_then(parse_pos) else {
            continue;
        };
        let col = ((x_pct / 100.0) * (SPRITE_COLS - 1) as f32).round() as u32;
        let row = ((y_pct / 100.0) * (SPRITE_ROWS - 1) as f32).round() as u32;
        if col >= SPRITE_COLS || row >= SPRITE_ROWS {
            continue;
        }
        let filtered = out.len();
        let group = group_of(filtered);
        let cell = rc_map
            .get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(parse_cell);
        let insert = insert_for(key, cell, &cell_to_uni);
        out.push(EmojiItem {
            insert,
            tip: key.clone(),
            col: col as u16,
            row: row as u16,
            group,
        });
    }
    out
}

fn group_of(filtered_index: usize) -> u8 {
    let mut g = 0u8;
    for (i, start) in GROUP_POS.iter().enumerate() {
        if filtered_index >= *start {
            g = i as u8;
        }
    }
    g
}

fn insert_for(
    key: &str,
    cell: Option<(u16, u16)>,
    cell_to_uni: &HashMap<(u16, u16), String>,
) -> String {
    if is_unicode_shortcode(key) {
        if let Some(cell) = cell {
            if let Some(uni) = cell_to_uni.get(&cell) {
                return uni.clone();
            }
        }
    }
    key.to_owned()
}

fn is_unicode_shortcode(key: &str) -> bool {
    key.len() > 3
        && key.starts_with(':')
        && key.ends_with(':')
        && key.as_bytes()[1..key.len() - 1]
            .iter()
            .any(u8::is_ascii_alphabetic)
}

fn parse_cell(raw: &str) -> Option<(u16, u16)> {
    let mut parts = raw.split_whitespace();
    let row = parts.next()?.parse().ok()?;
    let col = parts.next()?.parse().ok()?;
    Some((row, col))
}

fn load_json_object(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn parse_pos(raw: &str) -> Option<(f32, f32)> {
    let mut parts = raw.split_whitespace();
    let x = parts.next()?.trim_end_matches('%').parse().ok()?;
    let y = parts.next()?.trim_end_matches('%').parse().ok()?;
    Some((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_electron_categories() {
        let dir = emoji_dir();
        assert!(
            dir.join("emoji-rc.json").is_file(),
            "missing {}",
            dir.display()
        );
        assert!(
            dir.join(SPRITE_FILE).is_file(),
            "missing sprite in {}",
            dir.display()
        );
        let all = catalog();
        assert!(all.len() >= 1000, "catalog {}", all.len());
        let zalo = group_items(0).count();
        let people = group_items(1).count();
        let flags = group_items(8).count();
        assert!(zalo >= 70, "zalo {zalo}");
        assert!(people >= 100, "people {people}");
        assert!(flags >= 50, "flags {flags}");
        assert_eq!(
            group_items(0).count()
                + group_items(1).count()
                + group_items(2).count()
                + group_items(3).count()
                + group_items(4).count()
                + group_items(5).count()
                + group_items(6).count()
                + group_items(7).count()
                + group_items(8).count(),
            all.len()
        );
        let smile = all.iter().find(|i| i.tip == ":)" || i.insert == ":)");
        let smile = smile.expect("classic :) missing");
        assert_eq!(smile.group, 0);
        assert_eq!((smile.col, smile.row), (41, 7));
        assert!(tile_image(smile).is_some(), "classic :) tile must crop from sprite");
        let uni = all
            .iter()
            .find(|i| i.group == 1)
            .expect("unicode emoji missing");
        assert!(tile_image(uni).is_some(), "unicode emoji tile must crop from sprite");
        assert_eq!(thumb_key(smile), "emoji-41-7");
    }

    #[test]
    fn pictogram_split_finds_classic_and_keeps_text() {
        let _ = catalog();
        let parts = split_pictograms("hello :) world");
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, PictogramPart::Tile(t) if t.insert == ":)")),
            "{parts:?}"
        );
        assert!(contains_pictogram("a :) b"));
        assert!(!contains_pictogram("xin chao"));
    }

    #[test]
    fn sprite_pos_maps_smile_to_column_41() {
        assert_eq!(parse_pos("82.00% 17.50%"), Some((82.0, 17.5)));
        let col = ((82.0 / 100.0) * (SPRITE_COLS - 1) as f32).round() as u32;
        let row = ((17.5 / 100.0) * (SPRITE_ROWS - 1) as f32).round() as u32;
        assert_eq!((col, row), (41, 7));
    }
}
