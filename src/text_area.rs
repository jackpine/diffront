use crate::theme::Theme;
use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window,
    WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

actions!(
    text_area,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        KillToLineEnd,
        Yank,
        Transpose,
        Newline,
        OpenLine,
        Left,
        Right,
        Up,
        Down,
        WordLeft,
        WordRight,
        LineStart,
        LineEnd,
        DocumentStart,
        DocumentEnd,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectWordLeft,
        SelectWordRight,
        SelectLineStart,
        SelectLineEnd,
        SelectDocumentStart,
        SelectDocumentEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
    ]
);

const CONTEXT: &str = "TextArea";

#[derive(Clone, Copy)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

/// The text and selection to restore on undo or redo.
struct Snapshot {
    content: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

/// A multi-line, soft-wrapping plain text input.
pub struct TextArea {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    layout: Option<TextLayout>,
    is_selecting: bool,
    kill_buffer: String,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// Where typing continues the previous undo step rather than starting a new one.
    typing_end: Option<usize>,
}

impl TextArea {
    pub fn new(content: String, placeholder: &'static str, cx: &mut Context<Self>) -> Self {
        let end = content.len();
        Self {
            focus_handle: cx.focus_handle(),
            content: content.into(),
            placeholder: placeholder.into(),
            selected_range: end..end,
            selection_reversed: false,
            marked_range: None,
            layout: None,
            is_selecting: false,
            kill_buffer: String::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            typing_end: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.replace(0..self.content.len(), text, cx);
    }

    pub fn bind_keys(cx: &mut App) {
        let word = if cfg!(target_os = "macos") {
            "alt"
        } else {
            "ctrl"
        };
        let context = Some(CONTEXT);
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, context),
            KeyBinding::new("shift-backspace", Backspace, context),
            KeyBinding::new("delete", Delete, context),
            KeyBinding::new(&format!("{word}-backspace"), DeleteWordLeft, context),
            KeyBinding::new(&format!("{word}-delete"), DeleteWordRight, context),
            KeyBinding::new("shift-enter", Newline, context),
            KeyBinding::new("left", Left, context),
            KeyBinding::new("right", Right, context),
            KeyBinding::new("up", Up, context),
            KeyBinding::new("down", Down, context),
            KeyBinding::new(&format!("{word}-left"), WordLeft, context),
            KeyBinding::new(&format!("{word}-right"), WordRight, context),
            KeyBinding::new("home", LineStart, context),
            KeyBinding::new("end", LineEnd, context),
            KeyBinding::new("shift-left", SelectLeft, context),
            KeyBinding::new("shift-right", SelectRight, context),
            KeyBinding::new("shift-up", SelectUp, context),
            KeyBinding::new("shift-down", SelectDown, context),
            KeyBinding::new(&format!("{word}-shift-left"), SelectWordLeft, context),
            KeyBinding::new(&format!("{word}-shift-right"), SelectWordRight, context),
            KeyBinding::new("shift-home", SelectLineStart, context),
            KeyBinding::new("shift-end", SelectLineEnd, context),
            KeyBinding::new("secondary-a", SelectAll, context),
            KeyBinding::new("secondary-c", Copy, context),
            KeyBinding::new("secondary-x", Cut, context),
            KeyBinding::new("secondary-v", Paste, context),
            KeyBinding::new("secondary-z", Undo, context),
            KeyBinding::new("secondary-shift-z", Redo, context),
        ]);
        #[cfg(target_os = "macos")]
        cx.bind_keys([
            KeyBinding::new("cmd-left", LineStart, context),
            KeyBinding::new("cmd-right", LineEnd, context),
            KeyBinding::new("cmd-shift-left", SelectLineStart, context),
            KeyBinding::new("cmd-shift-right", SelectLineEnd, context),
            KeyBinding::new("cmd-up", DocumentStart, context),
            KeyBinding::new("cmd-down", DocumentEnd, context),
            KeyBinding::new("cmd-shift-up", SelectDocumentStart, context),
            KeyBinding::new("cmd-shift-down", SelectDocumentEnd, context),
            KeyBinding::new("alt-up", LineStart, context),
            KeyBinding::new("alt-down", LineEnd, context),
            KeyBinding::new("alt-shift-up", SelectLineStart, context),
            KeyBinding::new("alt-shift-down", SelectLineEnd, context),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, context),
            KeyBinding::new("cmd-delete", DeleteToLineEnd, context),
            KeyBinding::new("ctrl-a", LineStart, context),
            KeyBinding::new("ctrl-e", LineEnd, context),
            KeyBinding::new("ctrl-b", Left, context),
            KeyBinding::new("ctrl-f", Right, context),
            KeyBinding::new("ctrl-p", Up, context),
            KeyBinding::new("ctrl-n", Down, context),
            KeyBinding::new("ctrl-shift-a", SelectLineStart, context),
            KeyBinding::new("ctrl-shift-e", SelectLineEnd, context),
            KeyBinding::new("ctrl-shift-b", SelectLeft, context),
            KeyBinding::new("ctrl-shift-f", SelectRight, context),
            KeyBinding::new("ctrl-shift-p", SelectUp, context),
            KeyBinding::new("ctrl-shift-n", SelectDown, context),
            KeyBinding::new("ctrl-h", Backspace, context),
            KeyBinding::new("ctrl-d", Delete, context),
            KeyBinding::new("ctrl-k", KillToLineEnd, context),
            KeyBinding::new("ctrl-y", Yank, context),
            KeyBinding::new("ctrl-t", Transpose, context),
            KeyBinding::new("ctrl-o", OpenLine, context),
        ]);
        #[cfg(not(target_os = "macos"))]
        cx.bind_keys([
            KeyBinding::new("ctrl-home", DocumentStart, context),
            KeyBinding::new("ctrl-end", DocumentEnd, context),
            KeyBinding::new("ctrl-shift-home", SelectDocumentStart, context),
            KeyBinding::new("ctrl-shift-end", SelectDocumentEnd, context),
            KeyBinding::new("ctrl-y", Redo, context),
        ]);
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_cursor(&mut self, motion: Motion, cx: &mut Context<Self>) {
        let offset = match motion {
            Motion::Left if !self.selected_range.is_empty() => self.selected_range.start,
            Motion::Right if !self.selected_range.is_empty() => self.selected_range.end,
            _ => self.offset_after(motion),
        };
        self.move_to(offset, cx);
    }

    fn select_with(&mut self, motion: Motion, cx: &mut Context<Self>) {
        self.select_to(self.offset_after(motion), cx);
    }

    fn offset_after(&self, motion: Motion) -> usize {
        let cursor = self.cursor_offset();
        match motion {
            Motion::Left => self.previous_boundary(cursor),
            Motion::Right => self.next_boundary(cursor),
            Motion::WordLeft => self.previous_word_start(cursor),
            Motion::WordRight => self.next_word_end(cursor),
            Motion::LineStart => self.content[..cursor].rfind('\n').map_or(0, |ix| ix + 1),
            Motion::LineEnd => self.content[cursor..]
                .find('\n')
                .map_or(self.content.len(), |ix| cursor + ix),
            Motion::Up => self.offset_on_adjacent_row(cursor, -1.),
            Motion::Down => self.offset_on_adjacent_row(cursor, 1.),
            Motion::DocumentStart => 0,
            Motion::DocumentEnd => self.content.len(),
        }
    }

    fn offset_on_adjacent_row(&self, cursor: usize, direction: f32) -> usize {
        let Some(layout) = &self.layout else {
            return cursor;
        };
        let position = layout.position_for_index(cursor);
        let row_center = position.y + layout.line_height * (direction + 0.5);
        if row_center < px(0.) {
            0
        } else if row_center > layout.height() {
            self.content.len()
        } else {
            self.valid_offset(layout.index_for_position(point(position.x, row_center)))
        }
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(ix, _)| (ix < offset).then_some(ix))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(ix, _)| (ix > offset).then_some(ix))
            .unwrap_or(self.content.len())
    }

    fn previous_word_start(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .rev()
            .find(|(ix, word)| *ix < offset && !word.trim().is_empty())
            .map_or(0, |(ix, _)| ix)
    }

    fn next_word_end(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .find(|(ix, word)| ix + word.len() > offset && !word.trim().is_empty())
            .map_or(self.content.len(), |(ix, word)| ix + word.len())
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
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

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let continues_typing = range.is_empty() && self.typing_end == Some(range.start);
        if !continues_typing {
            self.undo_stack.push(self.snapshot());
            self.redo_stack.clear();
        }
        self.typing_end = (!text.is_empty() && !text.ends_with(char::is_whitespace))
            .then_some(range.start + text.len());
        self.content = format!(
            "{}{text}{}",
            &self.content[..range.start],
            &self.content[range.end..]
        )
        .into();
        self.move_to(range.start + text.len(), cx);
    }

    fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        self.replace(self.selected_range.clone(), text, cx);
    }

    fn delete_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(offset, cx);
        }
        self.insert("", cx);
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    fn restore(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.content = snapshot.content;
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked_range = None;
        self.typing_end = None;
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.redo_stack.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    fn redo(&mut self, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    /// Deletes to the end of the line, or joins the next line when already there, saving what was
    /// deleted for `yank`.
    fn kill_to_line_end(&mut self, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            let line_end = self.offset_after(Motion::LineEnd);
            let end = if line_end == cursor {
                self.next_boundary(cursor)
            } else {
                line_end
            };
            self.select_to(end, cx);
        }
        self.kill_buffer = self.content[self.selected_range.clone()].to_string();
        self.insert("", cx);
    }

    fn yank(&mut self, cx: &mut Context<Self>) {
        let killed = self.kill_buffer.clone();
        self.insert(&killed, cx);
    }

    /// Swaps the characters around the cursor, or the two before it at the end of a line.
    fn transpose(&mut self, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            return;
        }
        let mut middle = self.cursor_offset();
        if self.offset_after(Motion::LineEnd) == middle {
            middle = self.previous_boundary(middle);
        }
        let start = self.previous_boundary(middle);
        let end = self.next_boundary(middle);
        if start == middle || middle == end {
            return;
        }
        let swapped = format!("{}{}", &self.content[middle..end], &self.content[start..middle]);
        self.replace(start..end, &swapped, cx);
    }

    fn open_line(&mut self, cx: &mut Context<Self>) {
        self.insert("\n", cx);
        self.move_to(self.cursor_offset() - 1, cx);
    }

    fn copy_selection(&self, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            let selection = self.content[self.selected_range.clone()].to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(selection));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.insert(&text, cx);
        }
    }

    fn index_for_mouse(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = &self.layout else {
            return 0;
        };
        self.valid_offset(layout.index_for_position(position - layout.bounds.origin))
    }

    /// Layouts can describe the placeholder or a previous frame's text, so their offsets may not
    /// fit the current content.
    fn valid_offset(&self, offset: usize) -> usize {
        self.content.floor_char_boundary(offset)
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        let index = self.index_for_mouse(event.position);
        if event.modifiers.shift {
            self.select_to(index, cx);
        } else {
            self.move_to(index, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse(event.position), cx);
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        Self::utf8_offset(&self.content, offset)
    }

    fn utf8_offset(text: &str, utf16_offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in text.chars() {
            if utf16_count >= utf16_offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..offset].chars().map(char::len_utf16).sum()
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn resolve_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone())
    }
}

impl EntityInputHandler for TextArea {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.resolve_range(range_utf16);
        self.marked_range = None;
        self.replace(range, new_text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.resolve_range(range_utf16);
        self.replace(range.clone(), new_text, cx);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        if let Some(selected) = new_selected_range_utf16 {
            let start = range.start + Self::utf8_offset(new_text, selected.start);
            let end = range.start + Self::utf8_offset(new_text, selected.end);
            self.selected_range = start..end;
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let position = layout.position_for_index(range.start);
        Some(Bounds::new(
            layout.bounds.origin + position,
            size(px(2.), layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.layout.as_ref()?;
        let index = self.index_for_mouse(point);
        Some(self.offset_to_utf16(index))
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(|this, _: &Backspace, _, cx| {
                this.delete_to(this.offset_after(Motion::Left), cx)
            }))
            .on_action(cx.listener(|this, _: &Delete, _, cx| {
                this.delete_to(this.offset_after(Motion::Right), cx)
            }))
            .on_action(cx.listener(|this, _: &DeleteWordLeft, _, cx| {
                this.delete_to(this.offset_after(Motion::WordLeft), cx)
            }))
            .on_action(cx.listener(|this, _: &DeleteWordRight, _, cx| {
                this.delete_to(this.offset_after(Motion::WordRight), cx)
            }))
            .on_action(cx.listener(|this, _: &DeleteToLineStart, _, cx| {
                this.delete_to(this.offset_after(Motion::LineStart), cx)
            }))
            .on_action(cx.listener(|this, _: &DeleteToLineEnd, _, cx| {
                this.delete_to(this.offset_after(Motion::LineEnd), cx)
            }))
            .on_action(cx.listener(|this, _: &KillToLineEnd, _, cx| this.kill_to_line_end(cx)))
            .on_action(cx.listener(|this, _: &Yank, _, cx| this.yank(cx)))
            .on_action(cx.listener(|this, _: &Transpose, _, cx| this.transpose(cx)))
            .on_action(cx.listener(|this, _: &Newline, _, cx| this.insert("\n", cx)))
            .on_action(cx.listener(|this, _: &OpenLine, _, cx| this.open_line(cx)))
            .on_action(cx.listener(|this, _: &DocumentStart, _, cx| {
                this.move_cursor(Motion::DocumentStart, cx)
            }))
            .on_action(cx.listener(|this, _: &DocumentEnd, _, cx| {
                this.move_cursor(Motion::DocumentEnd, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectDocumentStart, _, cx| {
                this.select_with(Motion::DocumentStart, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectDocumentEnd, _, cx| {
                this.select_with(Motion::DocumentEnd, cx)
            }))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.undo(cx)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.redo(cx)))
            .on_action(cx.listener(|this, _: &Left, _, cx| this.move_cursor(Motion::Left, cx)))
            .on_action(cx.listener(|this, _: &Right, _, cx| this.move_cursor(Motion::Right, cx)))
            .on_action(cx.listener(|this, _: &Up, _, cx| this.move_cursor(Motion::Up, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.move_cursor(Motion::Down, cx)))
            .on_action(
                cx.listener(|this, _: &WordLeft, _, cx| this.move_cursor(Motion::WordLeft, cx)),
            )
            .on_action(
                cx.listener(|this, _: &WordRight, _, cx| this.move_cursor(Motion::WordRight, cx)),
            )
            .on_action(
                cx.listener(|this, _: &LineStart, _, cx| this.move_cursor(Motion::LineStart, cx)),
            )
            .on_action(
                cx.listener(|this, _: &LineEnd, _, cx| this.move_cursor(Motion::LineEnd, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLeft, _, cx| this.select_with(Motion::Left, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectRight, _, cx| this.select_with(Motion::Right, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.select_with(Motion::Up, cx)))
            .on_action(
                cx.listener(|this, _: &SelectDown, _, cx| this.select_with(Motion::Down, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectWordLeft, _, cx| {
                    this.select_with(Motion::WordLeft, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| {
                this.select_with(Motion::WordRight, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLineStart, _, cx| {
                this.select_with(Motion::LineStart, cx)
            }))
            .on_action(
                cx.listener(|this, _: &SelectLineEnd, _, cx| this.select_with(Motion::LineEnd, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy_selection(cx)))
            .on_action(cx.listener(|this, _: &Cut, _, cx| {
                this.copy_selection(cx);
                this.insert("", cx);
            }))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste(cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextAreaElement { area: cx.entity() })
    }
}

/// A hard line of text, placed vertically within the text area.
struct PlacedLine {
    start: usize,
    top: Pixels,
    line: WrappedLine,
}

/// Maps between byte offsets and positions relative to the text area's origin.
struct TextLayout {
    lines: Vec<PlacedLine>,
    line_height: Pixels,
    bounds: Bounds<Pixels>,
}

impl TextLayout {
    fn new(
        shaped: impl IntoIterator<Item = WrappedLine>,
        line_height: Pixels,
        bounds: Bounds<Pixels>,
    ) -> Self {
        let mut start = 0;
        let mut top = px(0.);
        let lines = shaped
            .into_iter()
            .map(|line| {
                let placed = PlacedLine { start, top, line };
                start += placed.line.len() + 1;
                top += placed.line.size(line_height).height;
                placed
            })
            .collect();
        Self {
            lines,
            line_height,
            bounds,
        }
    }

    fn height(&self) -> Pixels {
        self.lines.last().map_or(px(0.), |last| {
            last.top + last.line.size(self.line_height).height
        })
    }

    fn line_containing(&self, index: usize) -> Option<&PlacedLine> {
        self.lines.iter().rev().find(|line| line.start <= index)
    }

    fn position_for_index(&self, index: usize) -> Point<Pixels> {
        self.line_containing(index)
            .map_or(Point::default(), |placed| {
                let position = placed
                    .line
                    .position_for_index(index - placed.start, self.line_height)
                    .unwrap_or_default();
                point(position.x, placed.top + position.y)
            })
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let Some(placed) = self
            .lines
            .iter()
            .rev()
            .find(|line| line.top <= position.y)
            .or(self.lines.first())
        else {
            return 0;
        };
        let bottom_row = placed.line.size(self.line_height).height - self.line_height;
        let local_y = (position.y - placed.top).clamp(px(0.), bottom_row + self.line_height / 2.);
        let index = placed
            .line
            .closest_index_for_position(point(position.x.max(px(0.)), local_y), self.line_height)
            .unwrap_or_else(|nearest| nearest);
        placed.start + index
    }

    fn selection_quads(&self, range: &Range<usize>, color: gpui::Hsla) -> Vec<PaintQuad> {
        let mut quads = Vec::new();
        for placed in &self.lines {
            let line_end = placed.start + placed.line.len();
            if range.end < placed.start || range.start > line_end {
                continue;
            }
            let from = range.start.max(placed.start) - placed.start;
            let to = range.end.min(line_end) - placed.start;
            let from = placed
                .line
                .position_for_index(from, self.line_height)
                .unwrap_or_default();
            let to = placed
                .line
                .position_for_index(to, self.line_height)
                .unwrap_or_default();
            let rows = ((to.y - from.y) / self.line_height).round() as usize;
            for row in 0..=rows {
                let left = if row == 0 { from.x } else { px(0.) };
                let right = if row == rows {
                    to.x
                } else {
                    self.bounds.size.width
                };
                let top = placed.top + from.y + self.line_height * row;
                let origin = self.bounds.origin + point(left, top);
                let width = (right - left).max(px(4.));
                quads.push(fill(
                    Bounds::new(origin, size(width, self.line_height)),
                    color,
                ));
            }
        }
        quads
    }
}

struct TextAreaElement {
    area: Entity<TextArea>,
}

struct ShapeInput {
    text: SharedString,
    runs: Vec<TextRun>,
    font_size: Pixels,
    line_height: Pixels,
}

impl ShapeInput {
    fn shape(&self, width: Option<Pixels>, window: &Window) -> Vec<WrappedLine> {
        window
            .text_system()
            .shape_text(self.text.clone(), self.font_size, &self.runs, width, None)
            .map(|lines| lines.into_vec())
            .unwrap_or_default()
    }
}

impl TextAreaElement {
    fn shape_input(&self, window: &Window, cx: &App) -> ShapeInput {
        let area = self.area.read(cx);
        let style = window.text_style();
        let (text, color) = if area.content.is_empty() {
            (area.placeholder.clone(), cx.global::<Theme>().muted)
        } else {
            (area.content.clone(), style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match &area.marked_range {
            Some(marked) => vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text.len() - marked.end,
                    ..run
                },
            ],
            None => vec![run],
        };
        ShapeInput {
            text,
            runs,
            font_size: style.font_size.to_pixels(window.rem_size()),
            line_height: window.line_height(),
        }
    }
}

impl IntoElement for TextAreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextAreaElement {
    type RequestLayoutState = ();
    type PrepaintState = (Vec<PaintQuad>, Option<PaintQuad>);

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let input = self.shape_input(window, cx);
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, _| {
                let width = known.width.or(match available.width {
                    AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let rows: usize = input
                    .shape(width, window)
                    .iter()
                    .map(|line| line.wrap_boundaries().len() + 1)
                    .sum();
                size(width.unwrap_or_default(), input.line_height * rows)
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.shape_input(window, cx);
        let layout = TextLayout::new(
            input.shape(Some(bounds.size.width), window),
            input.line_height,
            bounds,
        );
        let theme = *cx.global::<Theme>();
        let area = self.area.read(cx);
        let selection = if area.selected_range.is_empty() {
            Vec::new()
        } else {
            layout.selection_quads(&area.selected_range, theme.text_selection)
        };
        let caret = area.selected_range.is_empty().then(|| {
            let origin = bounds.origin + layout.position_for_index(area.cursor_offset());
            fill(
                Bounds::new(origin, size(px(2.), input.line_height)),
                theme.accent,
            )
        });
        self.area.update(cx, |area, _| area.layout = Some(layout));
        (selection, caret)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        (selection, caret): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.area.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.area.clone()),
            cx,
        );
        for quad in selection.drain(..) {
            window.paint_quad(quad);
        }
        let area = self.area.read(cx);
        if let Some(layout) = &area.layout {
            let line_height = layout.line_height;
            let placed: Vec<_> = layout
                .lines
                .iter()
                .map(|placed| {
                    (
                        placed.line.clone(),
                        bounds.origin + point(px(0.), placed.top),
                    )
                })
                .collect();
            for (line, origin) in placed {
                line.paint(origin, line_height, Default::default(), None, window, cx)
                    .ok();
            }
        }
        if focus_handle.is_focused(window)
            && let Some(caret) = caret.take()
        {
            window.paint_quad(caret);
        }
    }
}
