//! Single-line IME field condensed from GPUI `examples/input.rs`.
//!
//! Enter emits [`LineEvent::Submit`]. Linux bindings include `ctrl-*` as well
//! as `cmd-*`. Search and composer each own one entity.

use std::ops::Range;

use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, Bounds, Context,
    CursorStyle, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ScrollDelta, ScrollWheelEvent,
    ShapedLine, SharedString, Style, Styled, TextRun, UTF16Selection, UnderlineStyle, Window,
};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    line_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Submit,
        Newline,
    ]
);

/// Emitted by [`LineInput`].
#[derive(Clone, Debug)]
pub enum LineEvent {
    /// Enter with the current contents (may be empty — callers ignore).
    Submit(String),
    /// Text changed by typing, IME, paste or clear.
    Change(String),
    /// Ctrl+V / "Paste" with an image on the clipboard.
    PasteImage(gpui::Image),
    /// Clipboard has both image and text (Excel / tables — honor `excelPaste`).
    PasteWithChoice {
        /// Clipboard image representation.
        image: gpui::Image,
        /// Clipboard text representation.
        text: String,
    },
}

/// Maximum visible lines painted inside a multiline composer input.
pub const MAX_VISIBLE_LINES: usize = 6;

/// One-line or multiline editor used by the search bar and the composer.
pub struct LineInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    /// Shaped visible visual lines `(utf8_byte_range, line)` for multiline mode.
    last_lines: Vec<(Range<usize>, ShapedLine)>,
    /// Total wrapped visual lines computed in the latest prepaint pass.
    last_total_lines: usize,
    /// First visual row currently painted in `last_lines`.
    last_win_start: usize,
    /// Clamped visible line count (`1..=MAX_VISIBLE_LINES`) from the latest wrap pass.
    wrapped_line_count: usize,
    /// Optional user scroll offset (cleared whenever cursor moves or text changes).
    manual_scroll_row: Option<usize>,
    last_line_h: Pixels,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// Password / PIN field: draw `*` per byte (keeps offsets aligned).
    masked: bool,
    /// Preserve `\n` on paste, soft-wrap to width, and allow `Shift+Enter` newlines (composer).
    multiline: bool,
    /// Known display names in the open thread for highlighting `@mentions` in blue text.
    mention_names: Vec<String>,
}

impl LineInput {
    /// Masked field for codes and PINs.
    pub fn new_masked(cx: &mut Context<Self>, placeholder: impl Into<SharedString>) -> Self {
        let mut input = Self::new(cx, placeholder);
        input.masked = true;
        input
    }

    /// Multiline composer field (preserves newlines on paste & `Shift+Enter`).
    pub fn new_multiline(cx: &mut Context<Self>, placeholder: impl Into<SharedString>) -> Self {
        let mut input = Self::new(cx, placeholder);
        input.multiline = true;
        input
    }

    /// Empty field with `placeholder`.
    pub fn new(cx: &mut Context<Self>, placeholder: impl Into<SharedString>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_lines: Vec::new(),
            last_total_lines: 1,
            last_win_start: 0,
            wrapped_line_count: 1,
            manual_scroll_row: None,
            last_line_h: px(22.0),
            last_bounds: None,
            is_selecting: false,
            masked: false,
            multiline: false,
            mention_names: Vec::new(),
        }
    }

    /// Number of visible lines (1..=[`MAX_VISIBLE_LINES`]) for multiline height sizing.
    #[must_use]
    pub fn visible_line_count(&self) -> usize {
        if !self.multiline || self.content.is_empty() {
            return 1;
        }
        let hard = self.content.bytes().filter(|&b| b == b'\n').count() + 1;
        hard.max(self.wrapped_line_count).clamp(1, MAX_VISIBLE_LINES)
    }

    /// Change the hint shown while empty (no notify; call from a parent render).
    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>) {
        let placeholder = placeholder.into();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
        }
    }

    /// Update known conversation participant names for `@mention` highlighting
    /// (no notify; call from a parent render).
    pub fn set_mention_names(&mut self, names: Vec<String>) {
        if self.mention_names != names {
            self.mention_names = names;
        }
    }

    /// Current UTF-8 contents.
    #[must_use]
    pub fn text(&self) -> String {
        self.content.to_string()
    }

    /// Replace the contents, cursor at the end.
    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.reset();
        let len = text.len();
        self.content = text.into();
        self.selected_range = len..len;
        cx.notify();
    }

    /// Clear the field and emit [`LineEvent::Change`].
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.reset();
        cx.emit(LineEvent::Change(String::new()));
        cx.notify();
    }

    /// Append `text` at the cursor (emoji insert).
    pub fn insert_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, text, window, cx);
    }

    /// Transform the selected text (or the entire field when nothing is selected).
    pub fn transform_text(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&str) -> String) {
        if self.masked {
            return;
        }
        let range = if self.selected_range.is_empty() {
            0..self.content.len()
        } else {
            self.selected_range.clone()
        };
        let transformed = f(&self.content[range.clone()]);
        self.content =
            (self.content[0..range.start].to_owned() + &transformed + &self.content[range.end..])
                .into();
        let end = range.start + transformed.len();
        self.selected_range = end..end;
        self.marked_range.take();
        cx.emit(LineEvent::Change(self.content.to_string()));
        cx.notify();
    }

    fn reset(&mut self) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_layout = None;
        self.last_lines.clear();
        self.last_total_lines = 1;
        self.last_win_start = 0;
        self.wrapped_line_count = 1;
        self.manual_scroll_row = None;
        self.last_bounds = None;
        self.is_selecting = false;
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline || self.last_lines.len() <= 1 {
            return;
        }
        let cur = self.cursor_offset();
        let row = self.visible_row_for_cursor(cur);
        if row == 0 {
            self.move_to(0, cx);
            return;
        }
        let (cur_range, cur_line) = &self.last_lines[row];
        let x = cur_line.x_for_index(cur.saturating_sub(cur_range.start).min(cur_range.len()));
        let (prev_range, prev_line) = &self.last_lines[row - 1];
        let local = prev_line.closest_index_for_x(x);
        let next = (prev_range.start + local).min(prev_range.end);
        self.move_to(next, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline || self.last_lines.len() <= 1 {
            return;
        }
        let cur = self.cursor_offset();
        let row = self.visible_row_for_cursor(cur);
        if row + 1 >= self.last_lines.len() {
            self.move_to(self.content.len(), cx);
            return;
        }
        let (cur_range, cur_line) = &self.last_lines[row];
        let x = cur_line.x_for_index(cur.saturating_sub(cur_range.start).min(cur_range.len()));
        let (next_range, next_line) = &self.last_lines[row + 1];
        let local = next_line.closest_index_for_x(x);
        let next = (next_range.start + local).min(next_range.end);
        self.move_to(next, cx);
    }

    fn visible_row_for_cursor(&self, cursor: usize) -> usize {
        let ranges: Vec<Range<usize>> = self.last_lines.iter().map(|(r, _)| r.clone()).collect();
        cursor_visual_row(&ranges, &self.content, cursor)
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.multiline || self.last_total_lines <= MAX_VISIBLE_LINES {
            return;
        }
        let dy = match event.delta {
            ScrollDelta::Lines(pt) => -pt.y,
            ScrollDelta::Pixels(pt) => -f32::from(pt.y) / f32::from(self.last_line_h).max(16.0),
        };
        let step = if dy > 0.1 {
            1isize
        } else if dy < -0.1 {
            -1isize
        } else {
            return;
        };
        let max_start = self.last_total_lines.saturating_sub(MAX_VISIBLE_LINES) as isize;
        let base = self.manual_scroll_row.unwrap_or(self.last_win_start) as isize;
        let next = (base + step).clamp(0, max_start) as usize;
        if self.manual_scroll_row != Some(next) {
            self.manual_scroll_row = Some(next);
            cx.notify();
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        self.paste_clipboard(window, cx);
    }

    /// Image/text clipboard handling (honoring mixed Excel image+text when both exist).
    pub fn paste_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let image = item.entries().iter().find_map(|entry| {
            if let gpui::ClipboardEntry::Image(image) = entry {
                Some(image.clone())
            } else {
                None
            }
        });
        let text = item.text().filter(|t| !t.is_empty());
        match (image, text) {
            (Some(image), Some(text)) => {
                cx.emit(LineEvent::PasteWithChoice { image, text });
            }
            (Some(image), None) => {
                cx.emit(LineEvent::PasteImage(image));
            }
            (None, Some(text)) => {
                let normalized = if self.multiline {
                    text.replace("\r\n", "\n").replace('\r', "\n")
                } else {
                    text.replace("\r\n", " ").replace(['\n', '\r'], " ")
                };
                self.replace_text_in_range(None, &normalized, window, cx);
            }
            (None, None) => {}
        }
    }

    /// True when some text is selected (enables "Cut" / "Copy").
    #[must_use]
    pub fn has_selection(&self) -> bool {
        !self.selected_range.is_empty()
    }

    /// Context-menu "Copy".
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            crate::clip::copy_text(cx, self.content[self.selected_range.clone()].to_string());
        }
    }

    /// Context-menu "Cut".
    pub fn cut_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(cx);
        if !self.selected_range.is_empty() {
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    /// Context-menu "Select All" or double-click select-all.
    pub fn select_everything(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.masked {
            return;
        }
        if !self.selected_range.is_empty() {
            crate::clip::copy_text(cx, self.content[self.selected_range.clone()].to_string());
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.masked {
            return;
        }
        if !self.selected_range.is_empty() {
            crate::clip::copy_text(cx, self.content[self.selected_range.clone()].to_string());
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    /// Emits even when empty: the chat sends a pending photo with no caption.
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(LineEvent::Submit(self.content.to_string()));
    }

    /// `Shift+Enter` inserts a newline in multiline inputs (or submits in single-line inputs).
    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.replace_text_in_range(None, "\n", window, cx);
        } else {
            cx.emit(LineEvent::Submit(self.content.to_string()));
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if event.click_count >= 2 {
            self.is_selecting = false;
            self.select_everything(cx);
            return;
        }
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.is_selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.is_selecting = false;
            return;
        }
        self.select_to(self.index_for_mouse_position(event.position), cx);
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.manual_scroll_row = None;
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(bounds) = self.last_bounds.as_ref() else {
            return 0;
        };
        if !self.last_lines.is_empty() {
            let line_h = f32::from(self.last_line_h).max(1.0);
            if position.y < bounds.top() {
                return self.last_lines[0].0.start;
            }
            let row = ((f32::from(position.y - bounds.top()) / line_h).floor() as isize)
                .clamp(0, (self.last_lines.len() as isize) - 1) as usize;
            let (range, line) = &self.last_lines[row];
            let local = line.closest_index_for_x(position.x - bounds.left());
            return (range.start + local).min(range.end);
        }
        let Some(line) = self.last_layout.as_ref() else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EventEmitter<LineEvent> for LineInput {}

impl Focusable for LineInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for LineInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        self.manual_scroll_row = None;
        cx.emit(LineEvent::Change(self.content.to_string()));
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.manual_scroll_row = None;
        cx.emit(LineEvent::Change(self.content.to_string()));
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if !self.last_lines.is_empty() {
            let row = self.visible_row_for_cursor(range.start);
            let (line_range, shaped) = self.last_lines.get(row)?;
            let row_top = bounds.top() + self.last_line_h * (row as f32);
            let row_bot = row_top + self.last_line_h;
            let start_local = range.start.saturating_sub(line_range.start).min(line_range.len());
            let end_local = range.end.saturating_sub(line_range.start).min(line_range.len());
            return Some(Bounds::from_corners(
                point(bounds.left() + shaped.x_for_index(start_local), row_top),
                point(bounds.left() + shaped.x_for_index(end_local), row_bot),
            ));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if !self.last_lines.is_empty() {
            let utf8_index = self.index_for_mouse_position(point);
            return Some(self.offset_to_utf16(utf8_index));
        }
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct LineElement {
    input: Entity<LineInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    lines: Vec<(Range<usize>, ShapedLine)>,
    total_lines: usize,
    win_start: usize,
    line_h: Pixels,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
}

/// Split `text` into byte ranges of logical lines separated by `\n`.
fn logical_line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut all = Vec::new();
    let mut start = 0usize;
    for (idx, b) in text.bytes().enumerate() {
        if b == b'\n' {
            all.push(start..idx);
            start = idx + 1;
        }
    }
    all.push(start..text.len());
    all
}

/// Choose a word-boundary UTF-8 wrap offset inside `slice` given the maximum
/// byte index `max_bytes` (`0 < max_bytes < slice.len()`) that fits the width.
/// Keeps a trailing space on the upper line so the continuation starts cleanly
/// at column 0; falls back to grapheme boundary `max_bytes` for unbroken tokens.
fn choose_wrap_break(slice: &str, max_bytes: usize) -> usize {
    let clamped = max_bytes.min(slice.len());
    if clamped == 0 {
        return slice
            .grapheme_indices(true)
            .nth(1)
            .map(|(idx, _)| idx)
            .unwrap_or(slice.len());
    }
    // If the fit boundary lands right before a space, include that single space
    // on the current line so the next visual line never begins with a stray space.
    if let Some(ch) = slice[clamped..].chars().next() {
        if ch == ' ' || ch == '\t' {
            return clamped + ch.len_utf8();
        }
    }
    // Look backwards inside `&slice[..clamped]` for the last whitespace break.
    if let Some((ws_idx, ch)) = slice[..clamped]
        .char_indices()
        .rev()
        .find(|&(_, c)| c == ' ' || c == '\t')
    {
        let after = ws_idx + ch.len_utf8();
        if after > 0 {
            return after;
        }
    }
    // Unbroken long token (e.g. URL): keep at least one grapheme.
    if slice.is_char_boundary(clamped) {
        clamped
    } else {
        slice
            .grapheme_indices(true)
            .take_while(|(idx, _)| *idx < clamped)
            .last()
            .map(|(idx, g)| idx + g.len())
            .unwrap_or(slice.len())
    }
}

/// Returns which visual row index in `ranges` should display the caret for `cursor`.
/// On a soft-wrap seam (`ranges[i].end == ranges[i + 1].start`), prefers the
/// beginning of `ranges[i + 1]` so typing across a wrap immediately moves the
/// caret onto the new line.
fn cursor_visual_row(ranges: &[Range<usize>], text: &str, cursor: usize) -> usize {
    if ranges.is_empty() {
        return 0;
    }
    for (idx, r) in ranges.iter().enumerate() {
        if cursor < r.start {
            return idx;
        }
        if cursor < r.end {
            return idx;
        }
        if cursor == r.end {
            let next_same_seam = ranges
                .get(idx + 1)
                .is_some_and(|next| next.start == r.end && text.as_bytes().get(r.end) != Some(&b'\n'));
            if !next_same_seam {
                return idx;
            }
        }
    }
    ranges.len() - 1
}

/// Computes the visible row slice `win_start..win_end` (at most `MAX_VISIBLE_LINES`).
fn window_slice_for_rows(
    total_rows: usize,
    cur_row: usize,
    manual_scroll_row: Option<usize>,
) -> Range<usize> {
    if total_rows <= MAX_VISIBLE_LINES {
        return 0..total_rows;
    }
    let max_start = total_rows.saturating_sub(MAX_VISIBLE_LINES);
    let win_start = match manual_scroll_row {
        Some(sr) => sr.min(max_start),
        None => cur_row
            .saturating_sub(MAX_VISIBLE_LINES - 1)
            .min(max_start),
    };
    win_start..win_start + MAX_VISIBLE_LINES
}

/// Builds `TextRun`s for a segment `[seg_start..seg_end]` preserving any
/// intersecting `@mention` blue text color and IME `marked_range` underline.
fn runs_for_segment(
    seg_range: &Range<usize>,
    marked_range: Option<&Range<usize>>,
    mention_spans: &[Range<usize>],
    mention_color: gpui::Hsla,
    base_run: &TextRun,
) -> Vec<TextRun> {
    let seg_len = seg_range.len();
    if seg_len == 0 {
        return Vec::new();
    }
    let mut cuts: Vec<usize> = Vec::with_capacity(2 + mention_spans.len() * 2 + 2);
    cuts.push(seg_range.start);
    cuts.push(seg_range.end);
    for m in mention_spans {
        if m.end > seg_range.start && m.start < seg_range.end {
            cuts.push(m.start.max(seg_range.start));
            cuts.push(m.end.min(seg_range.end));
        }
    }
    if let Some(marked) = marked_range {
        if marked.end > seg_range.start && marked.start < seg_range.end {
            cuts.push(marked.start.max(seg_range.start));
            cuts.push(marked.end.min(seg_range.end));
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    if cuts.len() <= 2 {
        let in_mention = mention_spans
            .iter()
            .any(|m| m.start <= seg_range.start && seg_range.end <= m.end);
        let in_marked =
            marked_range.is_some_and(|mk| mk.start <= seg_range.start && seg_range.end <= mk.end);
        let color = if in_mention {
            mention_color
        } else {
            base_run.color
        };
        return vec![TextRun {
            len: seg_len,
            color,
            underline: in_marked.then(|| UnderlineStyle {
                color: Some(color),
                thickness: px(1.0),
                wavy: false,
            }),
            ..base_run.clone()
        }];
    }
    let mut out = Vec::with_capacity(cuts.len() - 1);
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b {
            continue;
        }
        let in_mention = mention_spans.iter().any(|m| m.start <= a && b <= m.end);
        let in_marked = marked_range.is_some_and(|mk| mk.start <= a && b <= mk.end);
        let color = if in_mention {
            mention_color
        } else {
            base_run.color
        };
        out.push(TextRun {
            len: b - a,
            color,
            underline: in_marked.then(|| UnderlineStyle {
                color: Some(color),
                thickness: px(1.0),
                wavy: false,
            }),
            ..base_run.clone()
        });
    }
    out
}

/// Shapes all logical paragraphs of `content`, soft-wrapping any line wider
/// than `max_w` at word boundaries using GPUI's exact text shaping metrics.
#[allow(clippy::too_many_arguments)]
fn shape_wrapped_lines(
    content: &str,
    max_w: Pixels,
    font_size: Pixels,
    marked_range: Option<&Range<usize>>,
    mention_spans: &[Range<usize>],
    mention_color: gpui::Hsla,
    base_run: &TextRun,
    window: &Window,
) -> Vec<(Range<usize>, ShapedLine)> {
    let mut out = Vec::new();
    let ts = window.text_system();
    for lr in logical_line_ranges(content) {
        if lr.is_empty() {
            let empty = ts.shape_line(SharedString::from(""), font_size, &[], None);
            out.push((lr, empty));
            continue;
        }
        let mut cur = lr.start;
        while cur < lr.end {
            let seg_range = cur..lr.end;
            let rest = &content[seg_range.clone()];
            let runs = runs_for_segment(
                &seg_range,
                marked_range,
                mention_spans,
                mention_color,
                base_run,
            );
            let shaped = ts.shape_line(SharedString::from(rest.to_owned()), font_size, &runs, None);
            if shaped.width <= max_w {
                out.push((seg_range, shaped));
                break;
            }
            let mut cut = shaped.closest_index_for_x(max_w);
            if cut > 0 && shaped.x_for_index(cut) > max_w {
                cut = rest
                    .grapheme_indices(true)
                    .rev()
                    .find_map(|(idx, _)| (idx < cut).then_some(idx))
                    .unwrap_or(0);
            }
            if cut >= rest.len() {
                out.push((seg_range, shaped));
                break;
            }
            let wrap_at = choose_wrap_break(rest, cut);
            let piece_range = cur..cur + wrap_at;
            let piece_str = &content[piece_range.clone()];
            let piece_runs = runs_for_segment(
                &piece_range,
                marked_range,
                mention_spans,
                mention_color,
                base_run,
            );
            let piece_shaped = ts.shape_line(
                SharedString::from(piece_str.to_owned()),
                font_size,
                &piece_runs,
                None,
            );
            out.push((piece_range, piece_shaped));
            cur += wrap_at;
        }
    }
    out
}

impl IntoElement for LineElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LineElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

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
        let vis_lines = self.input.read(cx).visible_line_count().max(1) as f32;
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * vis_lines).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let line_h = window.line_height();
        let font_size = style.font_size.to_pixels(window.rem_size());

        if input.multiline && !input.masked && !content.is_empty() {
            let max_w = (bounds.size.width - px(4.0)).max(px(40.0));
            let base_run = TextRun {
                len: 0,
                font: style.font(),
                color: style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let mention_spans =
                crate::chat_select::find_mention_spans(&content, &input.mention_names);
            let mention_color: gpui::Hsla = gpui::rgb(crate::color::link_fg()).into();
            let all_lines = shape_wrapped_lines(
                &content,
                max_w,
                font_size,
                input.marked_range.as_ref(),
                &mention_spans,
                mention_color,
                &base_run,
                window,
            );
            let total_lines = all_lines.len().max(1);
            let all_ranges: Vec<Range<usize>> = all_lines.iter().map(|(r, _)| r.clone()).collect();
            let cur_row = cursor_visual_row(&all_ranges, &content, cursor);
            let win = window_slice_for_rows(total_lines, cur_row, input.manual_scroll_row);
            let win_start = win.start;
            let visible_lines: Vec<(Range<usize>, ShapedLine)> =
                all_lines.into_iter().skip(win.start).take(win.len()).collect();

            let mut selections = Vec::new();
            let mut caret = None;
            for (row_idx, (range, shaped)) in visible_lines.iter().enumerate() {
                let row_top = bounds.top() + line_h * (row_idx as f32);
                let row_bot = row_top + line_h;
                let global_row = win_start + row_idx;
                if selected_range.is_empty() {
                    if global_row == cur_row {
                        let local = cursor.saturating_sub(range.start).min(range.len());
                        let cx_pos = shaped.x_for_index(local);
                        caret = Some(fill(
                            Bounds::new(point(bounds.left() + cx_pos, row_top), size(px(2.), line_h)),
                            caret_color(crate::color::is_dark()),
                        ));
                    }
                } else {
                    let s = selected_range.start.max(range.start);
                    let e = selected_range.end.min(range.end);
                    if s < e
                        || (selected_range.start <= range.start && selected_range.end > range.end)
                    {
                        let x0 = bounds.left() + shaped.x_for_index(s - range.start);
                        let x1 = if selected_range.end > range.end {
                            (bounds.left() + shaped.x_for_index(range.len()) + px(4.))
                                .min(bounds.right())
                        } else {
                            bounds.left() + shaped.x_for_index(e - range.start)
                        };
                        selections.push(fill(
                            Bounds::from_corners(point(x0, row_top), point(x1, row_bot)),
                            crate::color::selection_highlight(),
                        ));
                    }
                }
            }
            return PrepaintState {
                line: None,
                lines: visible_lines,
                total_lines,
                win_start,
                line_h,
                cursor: caret,
                selections,
            };
        }

        let (display_text, text_color) = if content.is_empty() {
            (
                input.placeholder.clone(),
                crate::color::rgb_hsla(crate::color::token_rgb("--text-placeholder", "#77859c")),
            )
        } else if input.masked {
            // Same byte length as the content so cursor offsets still match.
            ("*".repeat(content.len()).into(), style.color)
        } else {
            (content, style.color)
        };
        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len != 0)
            .collect()
        } else {
            vec![run]
        };
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);
        let cursor_pos = line.x_for_index(cursor);
        let (selections, cursor) = if selected_range.is_empty() {
            (
                Vec::new(),
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    caret_color(crate::color::is_dark()),
                )),
            )
        } else {
            (
                vec![fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    crate::color::selection_highlight(),
                )],
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            lines: Vec::new(),
            total_lines: 1,
            win_start: 0,
            line_h,
            cursor,
            selections,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }
        let line_h = prepaint.line_h;
        if !prepaint.lines.is_empty() {
            let lines = std::mem::take(&mut prepaint.lines);
            let total_lines = prepaint.total_lines;
            let win_start = prepaint.win_start;
            let new_vis = total_lines.clamp(1, MAX_VISIBLE_LINES);
            for (row_idx, (_, line)) in lines.iter().enumerate() {
                let origin = point(bounds.left(), bounds.top() + line_h * (row_idx as f32));
                let _ = line.paint(origin, line_h, window, cx);
            }
            if focus_handle.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
            let mut height_changed = false;
            self.input.update(cx, |input, _cx| {
                input.last_layout = None;
                input.last_lines = lines;
                input.last_total_lines = total_lines;
                input.last_win_start = win_start;
                input.last_line_h = line_h;
                input.last_bounds.replace(bounds);
                if input.wrapped_line_count != new_vis {
                    input.wrapped_line_count = new_vis;
                    height_changed = true;
                }
            });
            if height_changed {
                let entity = self.input.clone();
                window.defer(cx, move |window, cx| {
                    entity.update(cx, |_, cx| cx.notify());
                    window.refresh();
                });
            }
            return;
        }
        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, line_h, window, cx).unwrap();
        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }
        let mut height_changed = false;
        self.input.update(cx, |input, _cx| {
            input.last_lines.clear();
            input.last_total_lines = 1;
            input.last_win_start = 0;
            input.last_line_h = line_h;
            input.last_layout.replace(line);
            input.last_bounds.replace(bounds);
            if input.wrapped_line_count != 1 {
                input.wrapped_line_count = 1;
                height_changed = true;
            }
        });
        if height_changed {
            let entity = self.input.clone();
            window.defer(cx, move |window, cx| {
                entity.update(cx, |_, cx| cx.notify());
                window.refresh();
            });
        }
    }
}

impl gpui::Render for LineInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let multi_rows = self.multiline && self.visible_line_count() > 1;
        div()
            .flex()
            .flex_1()
            .w_full()
            .h_full()
            .when(!multi_rows, |el| el.items_center())
            .when(multi_rows, |el| el.items_start())
            .overflow_hidden()
            .key_context("LineInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::newline))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(LineElement { input: cx.entity() })
    }
}

/// Insertion bar. White on a dark surface, blue on a light one.
fn caret_color(dark: bool) -> gpui::Hsla {
    if dark {
        gpui::white()
    } else {
        gpui::blue()
    }
}

/// Register LineInput keys. Linux uses `ctrl-*`; `cmd-*` is kept for parity.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("LineInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("cmd-left", Home, ctx),
        KeyBinding::new("cmd-right", End, ctx),
        KeyBinding::new("shift-enter", Newline, ctx),
        KeyBinding::new("alt-enter", Newline, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new("ctrl-a", SelectAll, ctx),
        KeyBinding::new("cmd-a", SelectAll, ctx),
        KeyBinding::new("ctrl-c", Copy, ctx),
        KeyBinding::new("cmd-c", Copy, ctx),
        KeyBinding::new("ctrl-v", Paste, ctx),
        KeyBinding::new("cmd-v", Paste, ctx),
        KeyBinding::new("ctrl-x", Cut, ctx),
        KeyBinding::new("cmd-x", Cut, ctx),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_action_is_defined() {
        let _ = std::any::type_name::<super::Submit>();
        let _ = std::any::type_name::<super::LineEvent>();
    }

    #[test]
    fn caret_is_white_on_a_dark_surface() {
        assert_eq!(super::caret_color(true), gpui::white());
        assert_eq!(super::caret_color(false), gpui::blue());
    }

    #[test]
    fn choose_wrap_break_prefers_word_boundaries_for_vietnamese() {
        let s = "@All Ai đang dùng IOS phiên bản thấp thì do Safari không hỗ trợ";
        // Cut inside "phiên" -> wraps after "IOS " instead of splitting "phiên".
        let cut = s.find("phiên").unwrap() + 2;
        let brk = choose_wrap_break(s, cut);
        assert_eq!(&s[..brk], "@All Ai đang dùng IOS ");
        assert!(s[brk..].starts_with("phiên"));

        // Unbroken long URL still breaks cleanly at grapheme boundary without looping.
        let url = "https://example.com/very/long/unbroken/path/segment";
        assert_eq!(choose_wrap_break(url, 20), 20);
    }

    #[test]
    fn cursor_visual_row_prefers_next_line_at_soft_wrap_seam_only() {
        let text = "hello world\nnext";
        // Simulate soft wrap between "hello " (0..6) and "world" (6..11), hard wrap to "next" (12..16).
        let ranges = vec![0..6, 6..11, 12..16];
        assert_eq!(cursor_visual_row(&ranges, text, 0), 0);
        assert_eq!(cursor_visual_row(&ranges, text, 6), 1); // soft-wrap seam -> start of row 1
        assert_eq!(cursor_visual_row(&ranges, text, 11), 1); // before '\n' -> end of row 1
        assert_eq!(cursor_visual_row(&ranges, text, 12), 2); // after '\n' -> start of row 2
        assert_eq!(cursor_visual_row(&ranges, text, 16), 2);
    }

    #[test]
    fn window_slice_tracks_cursor_and_manual_scroll() {
        assert_eq!(window_slice_for_rows(3, 2, None), 0..3);
        assert_eq!(window_slice_for_rows(9, 8, None), 3..9);
        assert_eq!(window_slice_for_rows(9, 1, None), 0..6);
        assert_eq!(window_slice_for_rows(9, 8, Some(1)), 1..7);
        assert_eq!(window_slice_for_rows(9, 8, Some(99)), 3..9);
    }

    #[test]
    fn runs_for_segment_highlights_mentions_and_preserves_ime_underline() {
        let text = "@All @Nguyễn Hữu Thành chào bạn";
        let known = vec!["Nguyễn Hữu Thành".to_owned()];
        let spans = crate::chat_select::find_mention_spans(text, &known);
        let base_color: gpui::Hsla = gpui::rgb(0xF0F0F0).into();
        let mention_color: gpui::Hsla = gpui::rgb(0x66A6FF).into();
        let base = TextRun {
            len: 0,
            font: gpui::font("sans-serif"),
            color: base_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let ime_start = text.find("chào").unwrap();
        let ime_range = ime_start..ime_start + "chào".len();
        let runs = runs_for_segment(&(0..text.len()), Some(&ime_range), &spans, mention_color, &base);
        // Total byte length must match exactly.
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), text.len());
        // "@All" is blue text with no background.
        assert_eq!(runs[0].len, "@All".len());
        assert_eq!(runs[0].color, mention_color);
        assert!(runs[0].background_color.is_none());
        // Space between mentions is plain white.
        assert_eq!(runs[1].len, 1);
        assert_eq!(runs[1].color, base_color);
        // "@Nguyễn Hữu Thành" is blue text with no background.
        assert_eq!(runs[2].len, "@Nguyễn Hữu Thành".len());
        assert_eq!(runs[2].color, mention_color);
        assert!(runs[2].background_color.is_none());
        // IME marked slice carries underline on base color.
        let ime_run = runs.iter().find(|r| r.underline.is_some()).unwrap();
        assert_eq!(ime_run.len, "chào".len());
        assert_eq!(ime_run.color, base_color);
    }
}
