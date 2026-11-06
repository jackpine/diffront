use crate::diff::{Diff, Row};
use crate::display::{Column, DisplayList};
use std::ops::Range;

/// A position within a diff line's source text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPoint {
    pub row: usize,
    pub offset: usize,
}

/// Text highlighted in one column of the diff.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSelection {
    pub column: Column,
    pub anchor: TextPoint,
    pub head: TextPoint,
}

impl TextSelection {
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
