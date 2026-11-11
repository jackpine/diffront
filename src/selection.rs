use crate::diff::{Diff, Row};
use crate::display::{Column, DisplayList};
use std::ops::Range;

/// A position within a diff line's source text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPoint {
    pub row: usize,
    pub offset: usize,
}

/// How much a press selects: a double click selects words and a triple click selects lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unit {
    Character,
    Word,
    Line,
}

impl Unit {
    pub fn for_click_count(count: usize) -> Self {
        match count {
            0 | 1 => Self::Character,
            2 => Self::Word,
            _ => Self::Line,
        }
    }

    /// The span of this unit around `offset` in `text`.
    fn around(self, text: &str, offset: usize) -> Range<usize> {
        match self {
            Self::Character => offset..offset,
            Self::Line => 0..text.len(),
            Self::Word => {
                let pressed = text[offset..]
                    .chars()
                    .next()
                    .map(|ch| (offset, ch))
                    .or_else(|| text[..offset].char_indices().next_back());
                let Some((at, ch)) = pressed else {
                    return offset..offset;
                };
                let class = CharClass::of(ch);
                if class == CharClass::Other {
                    return at..at + ch.len_utf8();
                }
                let start = text[..at]
                    .char_indices()
                    .rev()
                    .take_while(|(_, ch)| CharClass::of(*ch) == class)
                    .last()
                    .map_or(at, |(ix, _)| ix);
                let end = text[at..]
                    .char_indices()
                    .find(|(_, ch)| CharClass::of(*ch) != class)
                    .map_or(text.len(), |(ix, _)| at + ix);
                start..end
            }
        }
    }
}

/// Characters a double click groups into one word.
#[derive(PartialEq)]
enum CharClass {
    Word,
    Space,
    Other,
}

impl CharClass {
    fn of(ch: char) -> Self {
        if ch.is_alphanumeric() || ch == '_' {
            Self::Word
        } else if ch.is_whitespace() {
            Self::Space
        } else {
            Self::Other
        }
    }
}

/// Text highlighted in one column of the diff.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSelection {
    pub column: Column,
    unit: Unit,
    /// The unit first pressed, which stays selected while dragging away from it.
    origin: (TextPoint, TextPoint),
    anchor: TextPoint,
    head: TextPoint,
}

impl TextSelection {
    pub fn unit(&self) -> Unit {
        self.unit
    }

    /// Selects the unit at `offset` in `text`, the line shown at `row`.
    pub fn new(column: Column, unit: Unit, row: usize, text: &str, offset: usize) -> Self {
        let span = unit.around(text, offset);
        let origin = (
            TextPoint {
                row,
                offset: span.start,
            },
            TextPoint {
                row,
                offset: span.end,
            },
        );
        Self {
            column,
            unit,
            origin,
            anchor: origin.0,
            head: origin.1,
        }
    }

    /// Extends to the unit at `offset` in `text`, the line shown at `row`; returns whether it changed.
    pub fn extend(&mut self, row: usize, text: &str, offset: usize) -> bool {
        let span = self.unit.around(text, offset);
        let start = TextPoint {
            row,
            offset: span.start,
        };
        let (anchor, head) = if start < self.origin.0 {
            (self.origin.1, start)
        } else {
            (
                self.origin.0,
                TextPoint {
                    row,
                    offset: span.end,
                },
            )
        };
        let changed = (anchor, head) != (self.anchor, self.head);
        self.anchor = anchor;
        self.head = head;
        changed
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The selected part of a line, if it's shown in the selected column.
    pub fn range_in(&self, column: Column, row: usize, line_len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered();
        if column != self.column || row < start.row || row > end.row {
            return None;
        }
        let from = if row == start.row { start.offset } else { 0 };
        let to = if row == end.row { end.offset } else { line_len };
        Some(from..to)
    }

    /// The selected source text, one line per row shown in the selected column.
    pub fn text(&self, diff: &Diff, display: &DisplayList) -> String {
        let lines: Vec<&str> = (0..display.len())
            .filter_map(|ix| self.column.row_in(display.item(ix)))
            .filter_map(|row| match &diff.rows[row] {
                Row::Line { line, .. } => self
                    .range_in(self.column, row, line.text.len())
                    .map(|range| &line.text[range]),
                _ => None,
            })
            .collect();
        lines.join("\n")
    }

    fn ordered(&self) -> (TextPoint, TextPoint) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::Layout;
    use crate::review::Review;

    const PATCH: &str = "diff --git a/a.rs b/a.rs\n\
                         @@ -1,3 +1,3 @@\n\
                         \x20first\n\
                         -old\n\
                         +new\n\
                         \x20last\n\
                         diff --git a/b.rs b/b.rs\n\
                         @@ -1 +1 @@\n\
                         -gone\n\
                         +here\n";
    const FIRST: usize = 2;
    const OLD: usize = 3;
    const NEW: usize = 4;
    const LAST: usize = 5;
    const HERE: usize = 9;

    fn review() -> Review {
        Review::new(String::new(), Diff::parse(PATCH))
    }

    fn selection(column: Column, anchor: (usize, usize), head: (usize, usize)) -> TextSelection {
        let point = |(row, offset)| TextPoint { row, offset };
        TextSelection {
            column,
            unit: Unit::Character,
            origin: (point(anchor), point(anchor)),
            anchor: point(anchor),
            head: point(head),
        }
    }

    fn text(selection: TextSelection, review: &Review, layout: Layout) -> String {
        selection.text(&review.diff, &DisplayList::new(review, layout))
    }

    #[test]
    fn a_column_copies_only_its_side_of_a_change() {
        let review = review();
        let left = selection(Column::Left, (FIRST, 2), (LAST, 2));
        let right = selection(Column::Right, (FIRST, 2), (LAST, 2));
        assert_eq!(text(left, &review, Layout::SideBySide), "rst\nold\nla");
        assert_eq!(text(right, &review, Layout::SideBySide), "rst\nnew\nla");
    }

    #[test]
    fn inline_copies_both_sides_of_a_change() {
        let review = review();
        let inline = selection(Column::Inline, (OLD, 1), (NEW, 2));
        assert_eq!(text(inline, &review, Layout::Inline), "ld\nne");
    }

    #[test]
    fn dragging_upward_selects_the_same_text() {
        let review = review();
        let upward = selection(Column::Inline, (NEW, 2), (OLD, 1));
        assert_eq!(text(upward, &review, Layout::Inline), "ld\nne");
    }

    #[test]
    fn collapsed_files_and_headers_are_left_out() {
        let mut review = review();
        let across_files = selection(Column::Right, (LAST, 0), (HERE, 4));
        assert_eq!(
            text(across_files, &review, Layout::SideBySide),
            "last\nhere"
        );
        review.collapsed.insert(0);
        assert_eq!(text(across_files, &review, Layout::SideBySide), "here");
    }

    fn selected(selection: TextSelection, text: &str) -> &str {
        let range = selection.range_in(selection.column, 0, text.len()).unwrap();
        &text[range]
    }

    #[test]
    fn a_double_click_selects_the_word_or_symbol_pressed() {
        let text = "let foo_bar = baz(1);";
        let word = |offset| {
            selected(
                TextSelection::new(Column::Inline, Unit::Word, 0, text, offset),
                text,
            )
        };
        assert_eq!(word(5), "foo_bar");
        assert_eq!(word(4), "foo_bar");
        assert_eq!(word(10), "foo_bar");
        assert_eq!(word(11), " ");
        assert_eq!(word(17), "(");
        assert_eq!(word(text.len()), ";");
        assert_eq!(word(0), "let");
        assert_eq!(
            selected(TextSelection::new(Column::Inline, Unit::Word, 0, "", 0), ""),
            ""
        );
    }

    #[test]
    fn a_triple_click_selects_the_whole_line() {
        let text = "  indented line";
        let line = TextSelection::new(Column::Inline, Unit::Line, 0, text, 5);
        assert_eq!(selected(line, text), text);
    }

    #[test]
    fn dragging_after_a_double_click_extends_by_whole_words_and_keeps_the_first() {
        let review = review();
        let mut words = TextSelection::new(Column::Right, Unit::Word, NEW, "new", 1);
        assert!(words.extend(LAST, "last", 1));
        assert_eq!(text(words, &review, Layout::SideBySide), "new\nlast");
        assert!(words.extend(FIRST, "first", 2));
        assert_eq!(text(words, &review, Layout::SideBySide), "first\nnew");
        assert!(!words.extend(FIRST, "first", 4));
    }

    #[test]
    fn range_is_limited_to_the_selected_column_and_rows() {
        let selected = selection(Column::Left, (OLD, 1), (LAST, 2));
        assert_eq!(selected.range_in(Column::Left, OLD, 3), Some(1..3));
        assert_eq!(selected.range_in(Column::Left, NEW, 3), Some(0..3));
        assert_eq!(selected.range_in(Column::Left, LAST, 4), Some(0..2));
        assert_eq!(selected.range_in(Column::Left, FIRST, 5), None);
        assert_eq!(selected.range_in(Column::Right, LAST, 4), None);
    }
}
