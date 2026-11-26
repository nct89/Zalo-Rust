//! Cross-platform mouse drag text selection for chat message bubbles.
//!
//! GPUI does not provide browser-style drag selection on plain `div().child(text)`.
//! This module renders selectable message text with [`gpui::ShapedLine`] (plus
//! inline emoji tiles), tracks live screen bounds for every visible text block,
//! paints selection highlight quads, and extracts exact UTF-8 slices across one
//! or multiple messages for `Ctrl+C` / `Cmd+C` and right-click Copy.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    fill, point, px, size, App, Bounds, Corners, CursorStyle, Element, ElementId, Font,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, IntoElement, LayoutId, Pixels, Point,
    RenderImage, ShapedLine, SharedString, Size, Style, TextRun, Window,
};

use crate::emoji::{self, PictogramPart};
use crate::media;

/// Minimum pointer movement (in logical pixels) before a press-and-move is
/// treated as a drag selection rather than a plain click.
pub(crate) const DRAG_THRESHOLD_PX: f32 = 3.0;

/// Unique key identifying one selectable text block inside a message bubble.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TextBlockKey {
    /// Message row id (`ChatMsg.id`).
    pub msg_id: u64,
    /// Sub-block index within the message (0 = primary body, 1+ = secondary text).
    pub part: u8,
}

/// Endpoint of a text selection inside a chat conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextAnchor {
    /// Block containing this endpoint.
    pub key: TextBlockKey,
    /// Monotonic top-to-bottom order key (`msg_idx * 8 + part`) within the thread.
    pub order: usize,
    /// UTF-8 byte offset within the block's logical text string.
    pub offset: usize,
}

impl TextAnchor {
    fn cmp_pos(&self, other: &Self) -> std::cmp::Ordering {
        self.order
            .cmp(&other.order)
            .then_with(|| self.offset.cmp(&other.offset))
    }
}

/// Active text selection state owned by [`crate::shell::ShellView`].
#[derive(Clone, Debug, Default)]
pub(crate) struct ChatTextSelection {
    /// Conversation id the selection belongs to.
    pub conv_id: Option<u64>,
    /// Selection start anchor.
    pub anchor: Option<TextAnchor>,
    /// Selection moving focus endpoint.
    pub focus: Option<TextAnchor>,
    /// True while left mouse button is held down during a text drag.
    pub dragging: bool,
    /// True once pointer moved past [`DRAG_THRESHOLD_PX`] or changed offset during drag.
    pub drag_moved: bool,
    /// Window-space position where the current mouse press started.
    pub press_pos: Option<Point<Pixels>>,
    /// Timestamp-free flag set on mouse-up if the gesture produced a drag selection,
    /// allowing `on_click` handlers (e.g. link open) on the same release to ignore it.
    pub suppress_next_click: bool,
}

impl ChatTextSelection {
    /// Clear all selection state.
    pub(crate) fn clear(&mut self) {
        self.conv_id = None;
        self.anchor = None;
        self.focus = None;
        self.dragging = false;
        self.drag_moved = false;
        self.press_pos = None;
        self.suppress_next_click = false;
    }

    /// True when a non-empty character range is currently highlighted.
    #[must_use]
    pub(crate) fn has_selection(&self) -> bool {
        match (self.anchor, self.focus) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        }
    }

    /// Ordered `(start, end)` endpoints when a non-empty selection is active.
    #[must_use]
    pub(crate) fn ordered(&self) -> Option<(TextAnchor, TextAnchor)> {
        let (a, b) = (self.anchor?, self.focus?);
        if a == b {
            return None;
        }
        if a.cmp_pos(&b).is_le() {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    /// Compute the selected UTF-8 byte range inside one text block, if any.
    #[must_use]
    pub(crate) fn range_for_block(
        &self,
        key: TextBlockKey,
        order: usize,
        text_len: usize,
    ) -> Option<Range<usize>> {
        let (start, end) = self.ordered()?;
        if start.key == key && end.key == key {
            let s = start.offset.min(text_len);
            let e = end.offset.min(text_len);
            return (s < e).then_some(s..e);
        }
        if order < start.order || order > end.order {
            return None;
        }
        if start.key == key {
            let s = start.offset.min(text_len);
            return (s < text_len).then_some(s..text_len);
        }
        if end.key == key {
            let e = end.offset.min(text_len);
            return (e > 0).then_some(0..e);
        }
        (text_len > 0).then_some(0..text_len)
    }

    /// Begin a press on a text block.
    pub(crate) fn begin_press(
        &mut self,
        conv_id: u64,
        hit: TextAnchor,
        pos: Point<Pixels>,
    ) {
        self.conv_id = Some(conv_id);
        self.anchor = Some(hit);
        self.focus = Some(hit);
        self.dragging = true;
        self.drag_moved = false;
        self.press_pos = Some(pos);
        self.suppress_next_click = false;
    }

    /// Set an explicit range inside one block (e.g. double-click word or triple-click block).
    pub(crate) fn select_range(
        &mut self,
        conv_id: u64,
        key: TextBlockKey,
        order: usize,
        range: Range<usize>,
    ) {
        self.conv_id = Some(conv_id);
        self.anchor = Some(TextAnchor {
            key,
            order,
            offset: range.start,
        });
        self.focus = Some(TextAnchor {
            key,
            order,
            offset: range.end,
        });
        self.dragging = false;
        self.drag_moved = true;
        self.press_pos = None;
        self.suppress_next_click = true;
    }

    /// Update the moving focus endpoint while dragging. Returns `true` if changed.
    pub(crate) fn update_drag(&mut self, hit: TextAnchor, pos: Point<Pixels>) -> bool {
        if !self.dragging {
            return false;
        }
        if !self.drag_moved {
            let moved_far = self.press_pos.is_some_and(|p| {
                let dx = f32::from(pos.x) - f32::from(p.x);
                let dy = f32::from(pos.y) - f32::from(p.y);
                dx.hypot(dy) >= DRAG_THRESHOLD_PX
            });
            if !moved_far && self.anchor == Some(hit) {
                return false;
            }
            self.drag_moved = true;
        }
        if self.focus == Some(hit) {
            return false;
        }
        self.focus = Some(hit);
        true
    }

    /// Finish the drag on mouse-up. Returns `true` if selection state changed.
    pub(crate) fn finish_drag(&mut self) -> bool {
        if !self.dragging {
            return false;
        }
        self.dragging = false;
        self.press_pos = None;
        if self.has_selection() {
            self.suppress_next_click = self.drag_moved;
            true
        } else {
            self.anchor = None;
            self.focus = None;
            self.drag_moved = false;
            self.suppress_next_click = false;
            true
        }
    }

    /// Consume the click-suppression flag after a drag release.
    pub(crate) fn take_suppress_click(&mut self) -> bool {
        let was = self.suppress_next_click;
        self.suppress_next_click = false;
        was
    }
}

/// One laid-out visual atom inside a row of a [`SelectableTextElement`].
#[derive(Clone)]
enum RowAtom {
    Text {
        range: Range<usize>,
        line: Rc<ShapedLine>,
        x: Pixels,
    },
    Tile {
        range: Range<usize>,
        image: Arc<RenderImage>,
        x: Pixels,
        width: Pixels,
        tile_px: Pixels,
    },
}

/// One visual row inside a laid-out text block.
#[derive(Clone)]
struct VisualRow {
    /// Logical UTF-8 byte range covered by this visual row.
    range: Range<usize>,
    atoms: Vec<RowAtom>,
    width: Pixels,
}

/// Live painted geometry for one visible text block in the current frame.
struct BlockLayout {
    key: TextBlockKey,
    order: usize,
    bounds: Bounds<Pixels>,
    line_h: Pixels,
    text_len: usize,
    rows: Vec<VisualRow>,
}

impl BlockLayout {
    fn hit_test(&self, pos: Point<Pixels>) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let rel_y = f32::from(pos.y - self.bounds.top());
        let lh = f32::from(self.line_h).max(1.0);
        if rel_y < 0.0 {
            return 0;
        }
        let row_idx = (rel_y / lh).floor() as usize;
        if row_idx >= self.rows.len() {
            return self.text_len;
        }
        let row = &self.rows[row_idx];
        let rel_x = pos.x - self.bounds.left();
        if rel_x <= px(0.0) {
            return row.range.start;
        }
        for atom in &row.atoms {
            match atom {
                RowAtom::Text { range, line, x } => {
                    let end_x = *x + line.width;
                    if rel_x <= end_x {
                        let local = line.closest_index_for_x((rel_x - *x).max(px(0.0)));
                        return (range.start + local).min(range.end);
                    }
                }
                RowAtom::Tile {
                    range, x, width, ..
                } => {
                    let end_x = *x + *width;
                    if rel_x <= end_x {
                        return if rel_x < *x + *width * 0.5 {
                            range.start
                        } else {
                            range.end
                        };
                    }
                }
            }
        }
        row.range.end
    }
}

/// Per-frame registry of visible selectable text blocks plus a text table for
/// extracting the selected string across one or more messages.
#[derive(Default)]
pub(crate) struct ChatSelectionRegistry {
    blocks: Vec<BlockLayout>,
    /// Retained `(order, text)` for blocks rendered in the current conversation so
    /// copying still succeeds even if a message scrolls slightly out of view.
    texts: HashMap<TextBlockKey, (usize, String)>,
    conv_id: Option<u64>,
}

/// Shared handle to [`ChatSelectionRegistry`] passed into [`SelectableTextElement`].
pub(crate) type SharedSelectionRegistry = Rc<RefCell<ChatSelectionRegistry>>;

impl ChatSelectionRegistry {
    /// Create an empty shared registry.
    #[must_use]
    pub(crate) fn new_shared() -> SharedSelectionRegistry {
        Rc::new(RefCell::new(Self::default()))
    }

    /// Reset per-frame geometry before painting the message list for `conv_id`.
    pub(crate) fn begin_frame(&mut self, conv_id: Option<u64>) {
        self.blocks.clear();
        if self.conv_id != conv_id {
            self.texts.clear();
            self.conv_id = conv_id;
        }
    }

    fn register(&mut self, layout: BlockLayout, text: String) {
        self.texts.insert(layout.key, (layout.order, text));
        if let Some(existing) = self.blocks.iter_mut().find(|b| b.key == layout.key) {
            *existing = layout;
        } else {
            self.blocks.push(layout);
        }
    }

    /// Hit-test a point strictly inside a visible text block's bounds.
    #[must_use]
    pub(crate) fn hit_test_exact(&self, pos: Point<Pixels>) -> Option<TextAnchor> {
        for block in self.blocks.iter().rev() {
            if block.bounds.contains(&pos) {
                return Some(TextAnchor {
                    key: block.key,
                    order: block.order,
                    offset: block.hit_test(pos),
                });
            }
        }
        None
    }

    /// Hit-test a point inside or within `(pad_x, pad_y)` of a visible text block.
    /// Used on `mouse_down` so sweeping across short `@mention` / phone bubbles
    /// from the surrounding bubble padding initiates drag-selection cleanly.
    #[must_use]
    pub(crate) fn hit_test_near(
        &self,
        pos: Point<Pixels>,
        pad_x: f32,
        pad_y: f32,
    ) -> Option<TextAnchor> {
        if let Some(exact) = self.hit_test_exact(pos) {
            return Some(exact);
        }
        let px_x = px(pad_x);
        let px_y = px(pad_y);
        for block in self.blocks.iter().rev() {
            let expanded = Bounds::from_corners(
                point(block.bounds.left() - px_x, block.bounds.top() - px_y),
                point(block.bounds.right() + px_x, block.bounds.bottom() + px_y),
            );
            if expanded.contains(&pos) {
                return Some(TextAnchor {
                    key: block.key,
                    order: block.order,
                    offset: block.hit_test(pos),
                });
            }
        }
        None
    }

    /// Hit-test any window position during an active drag (including gaps between
    /// bubbles, horizontal margins, and above/below the viewport).
    #[must_use]
    pub(crate) fn hit_test_nearest(
        &self,
        pos: Point<Pixels>,
        anchor: TextAnchor,
    ) -> Option<TextAnchor> {
        if let Some(exact) = self.hit_test_exact(pos) {
            return Some(exact);
        }
        if self.blocks.is_empty() {
            return None;
        }
        // Sort visible block indices top-to-bottom by logical order.
        let mut indices: Vec<usize> = (0..self.blocks.len()).collect();
        indices.sort_by_key(|&i| self.blocks[i].order);

        // 1. Pointer's Y falls inside some block's vertical span (mouse moved to left/right margin).
        let mut best_y_match: Option<&BlockLayout> = None;
        let mut best_dx = f32::INFINITY;
        for &i in &indices {
            let b = &self.blocks[i];
            if pos.y >= b.bounds.top() && pos.y <= b.bounds.bottom() {
                let dx = if pos.x < b.bounds.left() {
                    f32::from(b.bounds.left() - pos.x)
                } else if pos.x > b.bounds.right() {
                    f32::from(pos.x - b.bounds.right())
                } else {
                    0.0
                };
                if dx < best_dx {
                    best_dx = dx;
                    best_y_match = Some(b);
                }
            }
        }
        if let Some(b) = best_y_match {
            return Some(TextAnchor {
                key: b.key,
                order: b.order,
                offset: b.hit_test(pos),
            });
        }

        // 2. Above the first visible block -> start of first block.
        let first = &self.blocks[indices[0]];
        if pos.y < first.bounds.top() {
            return Some(TextAnchor {
                key: first.key,
                order: first.order,
                offset: 0,
            });
        }

        // 3. Below the last visible block -> end of last block.
        let last = &self.blocks[*indices.last().unwrap()];
        if pos.y > last.bounds.bottom() {
            return Some(TextAnchor {
                key: last.key,
                order: last.order,
                offset: last.text_len,
            });
        }

        // 4. In a vertical gap between two consecutive blocks.
        for pair in indices.windows(2) {
            let above = &self.blocks[pair[0]];
            let below = &self.blocks[pair[1]];
            if pos.y > above.bounds.bottom() && pos.y < below.bounds.top() {
                return if anchor.order <= above.order {
                    Some(TextAnchor {
                        key: above.key,
                        order: above.order,
                        offset: above.text_len,
                    })
                } else {
                    Some(TextAnchor {
                        key: below.key,
                        order: below.order,
                        offset: 0,
                    })
                };
            }
        }
        None
    }

    /// Look up the full logical text stored for `key`.
    #[must_use]
    pub(crate) fn block_text(&self, key: TextBlockKey) -> Option<&str> {
        self.texts.get(&key).map(|(_, s)| s.as_str())
    }

    /// Extract the selected text across one or multiple blocks, joining distinct
    /// blocks with `"\n"`.
    #[must_use]
    pub(crate) fn extract_selected(&self, sel: &ChatTextSelection) -> Option<String> {
        let (start, end) = sel.ordered()?;
        if start.key == end.key {
            let (_, text) = self.texts.get(&start.key)?;
            let s = clamp_char_boundary(text, start.offset);
            let e = clamp_char_boundary(text, end.offset);
            return (s < e).then(|| text[s..e].to_owned());
        }
        let mut entries: Vec<(usize, TextBlockKey, &str)> = self
            .texts
            .iter()
            .filter_map(|(&key, (order, text))| {
                (*order >= start.order && *order <= end.order).then_some((*order, key, text.as_str()))
            })
            .collect();
        entries.sort_by_key(|(order, _, _)| *order);
        let mut parts: Vec<&str> = Vec::with_capacity(entries.len());
        for (order, key, text) in entries {
            let Some(range) = sel.range_for_block(key, order, text.len()) else {
                continue;
            };
            let s = clamp_char_boundary(text, range.start);
            let e = clamp_char_boundary(text, range.end);
            if s < e {
                parts.push(&text[s..e]);
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }
}

/// Custom GPUI element that renders wrapped text (and optional inline emoji
/// tiles) with character-accurate drag selection highlight.
pub(crate) struct SelectableTextElement {
    key: TextBlockKey,
    order: usize,
    text: String,
    max_w: f32,
    font_px: f32,
    line_h: f32,
    color: Hsla,
    font: Font,
    semibold: bool,
    with_pictograms: bool,
    colored_spans: Vec<(Range<usize>, Hsla)>,
    interactive_spans: Vec<Range<usize>>,
    selectable: bool,
    selection_range: Option<Range<usize>>,
    registry: SharedSelectionRegistry,
}

/// Interactive inline span hit inside a chat bubble (`http(s)://...`, `@mention`, or VN phone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InlineHit {
    Url(String),
    Mention(String),
    Phone(String),
}

/// Find UTF-8 byte ranges of all `http://` and `https://` URLs inside `text`.
#[must_use]
pub(crate) fn find_url_spans(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < text.len() {
        let rest = &text[pos..];
        let next = match (rest.find("https://"), rest.find("http://")) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let start = pos + next;
        let mut end = text[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
            .map(|rel| start + rel)
            .unwrap_or(text.len());
        while end > start {
            let Some(ch) = text[start..end].chars().next_back() else {
                break;
            };
            if matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}') {
                end -= ch.len_utf8();
            } else {
                break;
            }
        }
        if end > start + 8 {
            out.push(start..end);
        }
        pos = end.max(start + 1);
    }
    out
}

/// Returns the URL or `@mention` span covering UTF-8 byte `offset` in `text`, if any.
#[must_use]
pub(crate) fn interactive_span_at(
    text: &str,
    offset: usize,
    known_names: &[String],
) -> Option<InlineHit> {
    for r in find_url_spans(text) {
        if offset >= r.start && offset <= r.end {
            return Some(InlineHit::Url(text[r].to_owned()));
        }
    }
    for r in find_mention_spans(text, known_names) {
        if offset >= r.start && offset <= r.end {
            let name = text[r].trim_start_matches('@').trim();
            if !name.is_empty() {
                return Some(InlineHit::Mention(name.to_owned()));
            }
        }
    }
    if let Some(phone) = media::vn_phone_line(text.trim()) {
        return Some(InlineHit::Phone(phone));
    }
    None
}

/// Find UTF-8 byte ranges of `@mention` spans inside `text`.
///
/// Matches known participant/group names (longest first, case-insensitive) and
/// falls back to single Unicode word tokens like `@Trường` or `@All`, while
/// ignoring email addresses (`foo@bar.com`) and inline commands (`@gif:cat`).
#[must_use]
pub(crate) fn find_mention_spans(text: &str, known_names: &[String]) -> Vec<Range<usize>> {
    if !text.contains('@') {
        return Vec::new();
    }
    let mut candidates: Vec<&str> = known_names
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    candidates.push("Tất cả");
    candidates.push("All");
    candidates.sort_by(|a, b| b.len().cmp(&a.len()));
    candidates.dedup();

    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < text.len() {
        let Some(rel) = text[pos..].find('@') else {
            break;
        };
        let at = pos + rel;
        let prev_ok = if at == 0 {
            true
        } else {
            text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '.' && c != '-')
        };
        if !prev_ok {
            pos = at + 1;
            continue;
        }
        let after = &text[at + 1..];
        if after.is_empty() {
            break;
        }
        let mut matched_end: Option<usize> = None;
        for &name in &candidates {
            if let Some(end_rel) = match_prefix_ci(after, name) {
                let boundary_ok = after[end_rel..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                if boundary_ok {
                    matched_end = Some(at + 1 + end_rel);
                    break;
                }
            }
        }
        if matched_end.is_none() {
            let mut chars = after.char_indices().peekable();
            if let Some((_, first)) = chars.peek().copied() {
                if first.is_alphabetic() {
                    let mut end_rel = 0usize;
                    for (idx, ch) in chars {
                        if ch.is_alphanumeric() || ch == '_' {
                            end_rel = idx + ch.len_utf8();
                        } else {
                            break;
                        }
                    }
                    if end_rel > 0 {
                        let tail = &after[end_rel..];
                        let is_cmd = tail.starts_with(':');
                        let is_domain = tail.starts_with('.')
                            && tail[1..].chars().next().is_some_and(|c| c.is_alphabetic());
                        if !is_cmd && !is_domain {
                            matched_end = Some(at + 1 + end_rel);
                        }
                    }
                }
            }
        }
        if let Some(end) = matched_end {
            out.push(at..end);
            pos = end;
        } else {
            pos = at + 1;
        }
    }
    out
}

/// Case-insensitive prefix match returning the exact UTF-8 byte end inside `haystack`.
fn match_prefix_ci(haystack: &str, needle: &str) -> Option<usize> {
    let mut h_chars = haystack.char_indices();
    let mut end_byte = 0usize;
    for nc in needle.chars() {
        let (idx, hc) = h_chars.next()?;
        if hc.to_lowercase().ne(nc.to_lowercase()) {
            return None;
        }
        end_byte = idx + hc.len_utf8();
    }
    Some(end_byte)
}

/// Split a text slice `abs_range` into contiguous `TextRun`s applying any
/// overlapping `colored_spans`.
fn runs_for_slice(
    abs_range: Range<usize>,
    base_run: &TextRun,
    colored_spans: &[(Range<usize>, Hsla)],
) -> Vec<TextRun> {
    if colored_spans.is_empty() || abs_range.is_empty() {
        return vec![TextRun {
            len: abs_range.len(),
            ..base_run.clone()
        }];
    }
    let mut runs = Vec::new();
    let mut cursor = abs_range.start;
    for (span, color) in colored_spans {
        if span.end <= cursor {
            continue;
        }
        if span.start >= abs_range.end {
            break;
        }
        let s = span.start.max(cursor);
        let e = span.end.min(abs_range.end);
        if s > cursor {
            runs.push(TextRun {
                len: s - cursor,
                ..base_run.clone()
            });
        }
        if e > s {
            runs.push(TextRun {
                len: e - s,
                color: *color,
                ..base_run.clone()
            });
            cursor = e;
        }
    }
    if cursor < abs_range.end {
        runs.push(TextRun {
            len: abs_range.end - cursor,
            ..base_run.clone()
        });
    }
    if runs.is_empty() {
        runs.push(TextRun {
            len: abs_range.len(),
            ..base_run.clone()
        });
    }
    runs
}

impl SelectableTextElement {
    /// Attach colored highlight spans (e.g., blue `@mention` ranges).
    #[must_use]
    pub(crate) fn with_colored_spans(mut self, spans: Vec<(Range<usize>, Hsla)>) -> Self {
        self.colored_spans = spans;
        self
    }

    /// Plain wrapped text block (no inline emoji sprite tiles).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn plain(
        key: TextBlockKey,
        order: usize,
        text: String,
        max_w: f32,
        font_px: f32,
        line_h: f32,
        color: Hsla,
        font: Font,
        semibold: bool,
        selectable: bool,
        selection_range: Option<Range<usize>>,
        registry: SharedSelectionRegistry,
    ) -> Self {
        Self {
            key,
            order,
            text,
            max_w,
            font_px,
            line_h,
            color,
            font,
            semibold,
            with_pictograms: false,
            colored_spans: Vec::new(),
            interactive_spans: Vec::new(),
            selectable,
            selection_range,
            registry,
        }
    }

    pub(crate) fn with_interactive_spans(mut self, spans: Vec<Range<usize>>) -> Self {
        self.interactive_spans = spans;
        self
    }

    /// Rich bubble text block supporting inline Zalo/Unicode emoji sprite tiles.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rich(
        key: TextBlockKey,
        order: usize,
        text: String,
        max_w: f32,
        font_px: f32,
        line_h: f32,
        color: Hsla,
        font: Font,
        selectable: bool,
        selection_range: Option<Range<usize>>,
        registry: SharedSelectionRegistry,
    ) -> Self {
        Self {
            key,
            order,
            text,
            max_w,
            font_px,
            line_h,
            color,
            font,
            semibold: false,
            with_pictograms: true,
            colored_spans: Vec::new(),
            interactive_spans: Vec::new(),
            selectable,
            selection_range,
            registry,
        }
    }

    fn build_rows(&self, window: &Window) -> (Vec<VisualRow>, Size<Pixels>) {
        let font_size = px(self.font_px);
        let line_h = px(self.line_h);
        let base_run = TextRun {
            len: 0,
            font: self.font.clone(),
            color: self.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let rows = if self.with_pictograms {
            self.build_pictogram_rows(window, font_size, &base_run)
        } else {
            self.build_plain_rows(window, font_size, &base_run)
        };
        let max_line_w = rows
            .iter()
            .map(|r| f32::from(r.width))
            .fold(0.0_f32, f32::max)
            .ceil();
        let total_h = (rows.len().max(1) as f32) * f32::from(line_h);
        (rows, size(px(max_line_w), px(total_h)))
    }

    fn build_plain_rows(
        &self,
        window: &Window,
        font_size: Pixels,
        base_run: &TextRun,
    ) -> Vec<VisualRow> {
        let ranges = wrap_text_ranges(&self.text, self.max_w, self.font_px, self.semibold);
        let mut rows = Vec::with_capacity(ranges.len());
        for range in ranges {
            let slice = &self.text[range.clone()];
            if slice.is_empty() {
                rows.push(VisualRow {
                    range,
                    atoms: Vec::new(),
                    width: px(0.0),
                });
            } else {
                let runs = runs_for_slice(range.clone(), base_run, &self.colored_spans);
                let line = window.text_system().shape_line(
                    SharedString::from(slice.to_owned()),
                    font_size,
                    &runs,
                    None,
                );
                let width = line.width;
                rows.push(VisualRow {
                    range: range.clone(),
                    atoms: vec![RowAtom::Text {
                        range,
                        line: Rc::new(line),
                        x: px(0.0),
                    }],
                    width,
                });
            }
        }
        rows
    }

    fn build_pictogram_rows(
        &self,
        window: &Window,
        font_size: Pixels,
        base_run: &TextRun,
    ) -> Vec<VisualRow> {
        let parts = split_pictogram_ranges(&self.text);
        let max_w = px(self.max_w);
        let tile_px = px(20.0);
        let tile_advance = px(22.0);

        let mut rows: Vec<VisualRow> = vec![VisualRow {
            range: 0..0,
            atoms: Vec::new(),
            width: px(0.0),
        }];
        let mut cursor_x = px(0.0);

        let push_new_row = |rows: &mut Vec<VisualRow>, cursor_x: &mut Pixels, start_offset: usize| {
            rows.push(VisualRow {
                range: start_offset..start_offset,
                atoms: Vec::new(),
                width: px(0.0),
            });
            *cursor_x = px(0.0);
        };

        for part in parts {
            match part {
                PictogramSpan::Text(span_range) => {
                    let span = &self.text[span_range.clone()];
                    let mut seg_start = span_range.start;
                    for (li, seg) in span.split('\n').enumerate() {
                        if li > 0 {
                            push_new_row(&mut rows, &mut cursor_x, seg_start);
                        }
                        let mut word_start = seg_start;
                        for word in seg.split_inclusive(' ') {
                            let wrapped = media::wrap_bubble_text(word, self.max_w);
                            let sub_ranges = map_wrapped_ranges(word, &wrapped);
                            for (wi, sub) in sub_ranges.into_iter().enumerate() {
                                let abs = (word_start + sub.start)..(word_start + sub.end);
                                if wi > 0 {
                                    push_new_row(&mut rows, &mut cursor_x, abs.start);
                                }
                                let chunk = &self.text[abs.clone()];
                                if chunk.is_empty() {
                                    continue;
                                }
                                let runs = runs_for_slice(abs.clone(), base_run, &self.colored_spans);
                                let line = window.text_system().shape_line(
                                    SharedString::from(chunk.to_owned()),
                                    font_size,
                                    &runs,
                                    None,
                                );
                                let w = line.width;
                                if cursor_x > px(0.0) && cursor_x + w > max_w {
                                    push_new_row(&mut rows, &mut cursor_x, abs.start);
                                }
                                let row = rows.last_mut().unwrap();
                                if row.atoms.is_empty() {
                                    row.range.start = abs.start;
                                }
                                row.range.end = abs.end;
                                row.atoms.push(RowAtom::Text {
                                    range: abs,
                                    line: Rc::new(line),
                                    x: cursor_x,
                                });
                                cursor_x += w;
                                row.width = cursor_x;
                            }
                            word_start += word.len();
                        }
                        seg_start += seg.len() + 1;
                    }
                }
                PictogramSpan::Tile(abs, item) => {
                    if let Some(image) = emoji::tile_image(item) {
                        if cursor_x > px(0.0) && cursor_x + tile_advance > max_w {
                            push_new_row(&mut rows, &mut cursor_x, abs.start);
                        }
                        let row = rows.last_mut().unwrap();
                        if row.atoms.is_empty() {
                            row.range.start = abs.start;
                        }
                        row.range.end = abs.end;
                        row.atoms.push(RowAtom::Tile {
                            range: abs,
                            image,
                            x: cursor_x,
                            width: tile_advance,
                            tile_px,
                        });
                        cursor_x += tile_advance;
                        row.width = cursor_x;
                    } else {
                        let chunk = &self.text[abs.clone()];
                        let run = TextRun {
                            len: chunk.len(),
                            ..base_run.clone()
                        };
                        let line = window.text_system().shape_line(
                            SharedString::from(chunk.to_owned()),
                            font_size,
                            &[run],
                            None,
                        );
                        let w = line.width;
                        if cursor_x > px(0.0) && cursor_x + w > max_w {
                            push_new_row(&mut rows, &mut cursor_x, abs.start);
                        }
                        let row = rows.last_mut().unwrap();
                        if row.atoms.is_empty() {
                            row.range.start = abs.start;
                        }
                        row.range.end = abs.end;
                        row.atoms.push(RowAtom::Text {
                            range: abs,
                            line: Rc::new(line),
                            x: cursor_x,
                        });
                        cursor_x += w;
                        row.width = cursor_x;
                    }
                }
            }
        }
        rows
    }
}

/// Cached layout computed in `request_layout` and reused in `prepaint`/`paint`.
pub(crate) struct SelectableLayoutState {
    rows: Vec<VisualRow>,
}

impl IntoElement for SelectableTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectableTextElement {
    type RequestLayoutState = SelectableLayoutState;
    type PrepaintState = (Option<Hitbox>, Vec<(Hitbox, Bounds<Pixels>)>);

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (rows, measured) = self.build_rows(window);
        let mut style = Style::default();
        style.size.width = measured.width.into();
        style.size.height = measured.height.into();
        let layout_id = window.request_layout(style, [], cx);
        (layout_id, SelectableLayoutState { rows })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let outer = if self.selectable {
            let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
            self.registry.borrow_mut().register(
                BlockLayout {
                    key: self.key,
                    order: self.order,
                    bounds,
                    line_h: px(self.line_h),
                    text_len: self.text.len(),
                    rows: layout.rows.clone(),
                },
                self.text.clone(),
            );
            Some(hitbox)
        } else {
            None
        };
        let mut interactive_hitboxes = Vec::new();
        if !self.interactive_spans.is_empty() {
            let line_h = px(self.line_h);
            for (i, row) in layout.rows.iter().enumerate() {
                let y = bounds.top() + line_h * (i as f32);
                for atom in &row.atoms {
                    let RowAtom::Text { range, line, x } = atom else {
                        continue;
                    };
                    for span in &self.interactive_spans {
                        let s = span.start.max(range.start);
                        let e = span.end.min(range.end);
                        if s < e {
                            let x0 = bounds.left() + *x + line.x_for_index(s - range.start);
                            let x1 = bounds.left() + *x + line.x_for_index(e - range.start);
                            if x1 > x0 {
                                let sub = Bounds::from_corners(point(x0, y), point(x1, y + line_h));
                                let hb = window.insert_hitbox(sub, HitboxBehavior::Normal);
                                interactive_hitboxes.push((hb, sub));
                            }
                        }
                    }
                }
            }
        }
        (outer, interactive_hitboxes)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (outer, interactive_hitboxes) = prepaint;
        if let Some(hitbox) = outer.as_ref() {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
        }
        for (hb, _sub) in interactive_hitboxes.iter() {
            window.set_cursor_style(CursorStyle::PointingHand, hb);
        }
        let line_h = px(self.line_h);
        let highlight = selection_bg(crate::color::is_dark());
        let sel = self.selection_range.as_ref();

        for (i, row) in layout.rows.iter().enumerate() {
            let y = bounds.top() + line_h * (i as f32);

            // 1. Paint selection background behind this visual row.
            if let Some(sel) = sel {
                if row.atoms.is_empty() {
                    // Blank line inside a multi-line selection: paint a small bar.
                    if sel.start <= row.range.start && sel.end > row.range.start {
                        window.paint_quad(fill(
                            Bounds::new(point(bounds.left(), y), size(px(5.0), line_h)),
                            highlight,
                        ));
                    }
                } else {
                    for atom in &row.atoms {
                        match atom {
                            RowAtom::Text { range, line, x } => {
                                let s = sel.start.max(range.start);
                                let e = sel.end.min(range.end);
                                if s < e {
                                    let x0 = bounds.left() + *x + line.x_for_index(s - range.start);
                                    let x1 = bounds.left() + *x + line.x_for_index(e - range.start);
                                    if x1 > x0 {
                                        window.paint_quad(fill(
                                            Bounds::from_corners(
                                                point(x0, y),
                                                point(x1, y + line_h),
                                            ),
                                            highlight,
                                        ));
                                    }
                                }
                            }
                            RowAtom::Tile {
                                range, x, width, ..
                            } => {
                                if sel.start < range.end && sel.end > range.start {
                                    let x0 = bounds.left() + *x;
                                    window.paint_quad(fill(
                                        Bounds::new(point(x0, y), size(*width, line_h)),
                                        highlight,
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            // 2. Paint text runs and inline emoji tiles.
            for atom in &row.atoms {
                match atom {
                    RowAtom::Text { line, x, .. } => {
                        let _ = line.paint(point(bounds.left() + *x, y), line_h, window, cx);
                    }
                    RowAtom::Tile {
                        image, x, tile_px, ..
                    } => {
                        let ty = y + (line_h - *tile_px) * 0.5;
                        let tile_bounds = Bounds::new(
                            point(bounds.left() + *x, ty),
                            size(*tile_px, *tile_px),
                        );
                        let _ = window.paint_image(
                            tile_bounds,
                            Corners::default(),
                            Arc::clone(image),
                            0,
                            false,
                        );
                    }
                }
            }
        }
    }
}

fn selection_bg(dark: bool) -> Hsla {
    if dark {
        gpui::hsla(0., 0., 1., 0.22)
    } else {
        gpui::rgba(0x00_68_ff_44).into()
    }
}

enum PictogramSpan {
    Text(Range<usize>),
    Tile(Range<usize>, &'static emoji::EmojiItem),
}

fn split_pictogram_ranges(text: &str) -> Vec<PictogramSpan> {
    let parts = emoji::split_pictograms(text);
    let mut out = Vec::with_capacity(parts.len());
    let mut pos = 0_usize;
    for part in parts {
        match part {
            PictogramPart::Text(s) => {
                let end = (pos + s.len()).min(text.len());
                out.push(PictogramSpan::Text(pos..end));
                pos = end;
            }
            PictogramPart::Tile(item) => {
                let len = item.insert.len();
                let end = (pos + len).min(text.len());
                out.push(PictogramSpan::Tile(pos..end, item));
                pos = end;
            }
        }
    }
    out
}

/// Wrap `text` using [`media::wrap_text_at`] and map every visual line back to
/// its exact UTF-8 byte range in `text` so copied selections never contain
/// synthetic soft-wrap newlines.
#[must_use]
pub(crate) fn wrap_text_ranges(
    text: &str,
    max_px: f32,
    font_px: f32,
    semibold: bool,
) -> Vec<Range<usize>> {
    let wrapped = media::wrap_text_at(text, max_px, font_px, semibold);
    map_wrapped_ranges(text, &wrapped)
}

/// Map each `\n`-delimited visual line of `wrapped` back to its byte range in `text`.
#[must_use]
pub(crate) fn map_wrapped_ranges(text: &str, wrapped: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut pos = 0_usize;

    for (i, vline) in wrapped.split('\n').enumerate() {
        if i > 0 {
            if bytes.get(pos) == Some(&b'\n') {
                pos += 1;
            } else {
                while bytes.get(pos) == Some(&b' ') {
                    pos += 1;
                }
            }
        }
        if !vline.is_empty() && !text[pos..].starts_with(vline) {
            while bytes.get(pos) == Some(&b' ') && !text[pos..].starts_with(vline) {
                pos += 1;
            }
        }
        let start = pos;
        let end = if text[pos..].starts_with(vline) {
            pos += vline.len();
            pos
        } else {
            // Defensive fallback: clamp to valid UTF-8 boundary.
            let target = clamp_char_boundary(text, (pos + vline.len()).min(text.len()));
            pos = target;
            target
        };
        out.push(start..end);
    }
    if out.is_empty() {
        out.push(0..0);
    }
    out
}

/// Return the UTF-8 byte range of the word containing `offset` (for double-click).
#[must_use]
pub(crate) fn word_bounds_at(text: &str, offset: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let idx = clamp_char_boundary(text, offset);
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';

    // Pick character at `idx`, or the previous character if `idx == text.len()`.
    let probe = text[idx..]
        .chars()
        .next()
        .or_else(|| text[..idx].chars().next_back());
    let Some(ch) = probe else {
        return 0..0;
    };
    let want_word = is_word(ch);

    let mut start = idx;
    for (b, c) in text[..idx].char_indices().rev() {
        if is_word(c) == want_word && c != '\n' {
            start = b;
        } else {
            break;
        }
    }
    let mut end = idx;
    for (b, c) in text[idx..].char_indices() {
        if is_word(c) == want_word && c != '\n' {
            end = idx + b + c.len_utf8();
        } else {
            break;
        }
    }
    start..end
}

fn clamp_char_boundary(text: &str, offset: usize) -> usize {
    let mut idx = offset.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_ranges_preserve_original_spaces_and_newlines() {
        let text = "Xin chào Việt Nam hôm nay trời rất đẹp\n\nDòng thứ ba";
        let ranges = wrap_text_ranges(text, 120.0, 15.0, false);
        assert!(ranges.len() >= 4);
        // Every range is valid UTF-8 and monotonically ordered.
        for pair in ranges.windows(2) {
            assert!(pair[0].end <= pair[1].start);
        }
        // Selecting from first char to last char reconstructs the exact original text.
        let start = ranges.first().unwrap().start;
        let end = ranges.last().unwrap().end;
        assert_eq!(&text[start..end], text);
    }

    #[test]
    fn wrap_ranges_keep_long_url_intact_on_copy() {
        let url = "https://example.com/some/very/long/path/document_final_v2.zip";
        let ranges = wrap_text_ranges(url, 140.0, 15.0, false);
        assert!(ranges.len() >= 2);
        let start = ranges.first().unwrap().start;
        let end = ranges.last().unwrap().end;
        assert_eq!(&url[start..end], url);
    }

    #[test]
    fn single_and_multi_block_selection_extraction() {
        let mut reg = ChatSelectionRegistry::default();
        let k1 = TextBlockKey { msg_id: 10, part: 0 };
        let k2 = TextBlockKey { msg_id: 11, part: 0 };
        let k3 = TextBlockKey { msg_id: 12, part: 0 };
        reg.texts.insert(k1, (0, "Xin chào bạn".into()));
        reg.texts.insert(k2, (8, "Hôm nay thế nào?".into()));
        reg.texts.insert(k3, (16, "Rất tốt!".into()));

        // Reverse drag inside a single Vietnamese message.
        let mut sel = ChatTextSelection::default();
        sel.anchor = Some(TextAnchor {
            key: k1,
            order: 0,
            offset: "Xin chào".len(),
        });
        sel.focus = Some(TextAnchor {
            key: k1,
            order: 0,
            offset: 4,
        });
        assert_eq!(reg.extract_selected(&sel).as_deref(), Some("chào"));

        // Drag across three messages (bottom-up).
        sel.anchor = Some(TextAnchor {
            key: k3,
            order: 16,
            offset: "Rất".len(),
        });
        sel.focus = Some(TextAnchor {
            key: k1,
            order: 0,
            offset: 4,
        });
        assert_eq!(
            reg.extract_selected(&sel).as_deref(),
            Some("chào bạn\nHôm nay thế nào?\nRất")
        );
    }

    #[test]
    fn word_bounds_handles_vietnamese_words() {
        let text = "Xin chào Việt Nam!";
        let pos = text.find("chào").unwrap() + 2;
        let r = word_bounds_at(text, pos);
        assert_eq!(&text[r], "chào");
    }

    #[test]
    fn find_mention_spans_highlights_names_and_ignores_emails() {
        let known = vec!["Trường Nguyễn".to_owned(), "Dũng Mobile".to_owned()];
        let s1 = "Em đi sớm 30p nha ghi nhận dùm em @Trường";
        let spans1 = find_mention_spans(s1, &known);
        assert_eq!(spans1.len(), 1);
        assert_eq!(&s1[spans1[0].clone()], "@Trường");

        let s2 = "@Trường Nguyễn kiểm tra @Dũng Mobile và @All nhé, mail a@b.com @gif:cat";
        let spans2 = find_mention_spans(s2, &known);
        let texts: Vec<&str> = spans2.iter().map(|r| &s2[r.clone()]).collect();
        assert_eq!(texts, vec!["@Trường Nguyễn", "@Dũng Mobile", "@All"]);

        let s3 = "Xem https://t.co/3nFfaCwtXg, hoặc hỏi @Trường Nguyễn nhé!";
        let url_off = s3.find("3nFfa").unwrap();
        let mention_off = s3.find("Nguyễn").unwrap();
        assert_eq!(
            interactive_span_at(s3, url_off, &known),
            Some(InlineHit::Url("https://t.co/3nFfaCwtXg".to_owned()))
        );
        assert_eq!(
            interactive_span_at(s3, mention_off, &known),
            Some(InlineHit::Mention("Trường Nguyễn".to_owned()))
        );
        assert_eq!(interactive_span_at(s3, 0, &known), None);
    }
}
