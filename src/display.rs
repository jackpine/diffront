use crate::diff::{LineKind, Row};
use crate::review::Review;

/// How diff lines are arranged on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layout {
    SideBySide,
    Inline,
}

impl Layout {
    pub fn toggled(self) -> Self {
        match self {
            Layout::SideBySide => Layout::Inline,
            Layout::Inline => Layout::SideBySide,
        }
    }
}

/// One entry in the scrolling list, referring to rows of the diff.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Item {
    /// A header, or a line in the inline layout, spanning the full width.
    Full(usize),
    Context(usize),
    Removed(usize),
    Added(usize),
    Changed {
        removed: usize,
        added: usize,
    },
}

impl Item {
    pub fn first_row(self) -> usize {
        match self {
            Item::Full(row) | Item::Context(row) | Item::Removed(row) | Item::Added(row) => row,
            Item::Changed { removed, .. } => removed,
        }
    }

    /// The rows shown in the left and right columns.
    pub fn sides(self) -> (Option<usize>, Option<usize>) {
        match self {
            Item::Full(_) => (None, None),
            Item::Context(row) => (Some(row), Some(row)),
            Item::Removed(row) => (Some(row), None),
            Item::Added(row) => (None, Some(row)),
            Item::Changed { removed, added } => (Some(removed), Some(added)),
        }
    }
}

/// The items on screen, leaving out the bodies of collapsed files.
pub struct DisplayList {
    items: Vec<Item>,
    item_of_row: Vec<usize>,
}

impl DisplayList {
    pub fn new(review: &Review, layout: Layout) -> Self {
        let items = Self::build_items(review, layout);
        let item_of_row = Self::index_rows(&items, review.diff.rows.len());
        Self { items, item_of_row }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn item(&self, ix: usize) -> Item {
        self.items[ix]
    }

    pub fn first_row_at(&self, ix: usize) -> Option<usize> {
        self.items.get(ix).map(|item| item.first_row())
    }

    /// The item showing `row`, or the next one shown after it.
    pub fn item_of(&self, row: usize) -> usize {
        self.item_of_row[row]
    }

    fn build_items(review: &Review, layout: Layout) -> Vec<Item> {
        let rows = &review.diff.rows;
        let mut items = Vec::new();
        let mut row = 0;
        while row < rows.len() {
            let file = rows[row].file();
            if review.collapsed.contains(&file) {
                items.push(Item::Full(row));
                row = review.diff.file_body(file).end;
                continue;
            }
            match (&rows[row], layout) {
                (Row::Line { line, .. }, Layout::SideBySide) => match line.kind {
                    LineKind::Context { .. } => {
                        items.push(Item::Context(row));
                        row += 1;
                    }
                    LineKind::Removed { .. } | LineKind::Added { .. } => {
                        row = Self::push_change_block(rows, row, &mut items);
                    }
                },
                _ => {
                    items.push(Item::Full(row));
                    row += 1;
                }
            }
        }
        items
    }

    /// Pairs a run of removed lines with the added lines after it, returning the row that follows.
    fn push_change_block(rows: &[Row], start: usize, items: &mut Vec<Item>) -> usize {
        let run_end = |from: usize, wanted: fn(LineKind) -> bool| {
            from + rows[from..]
                .iter()
                .take_while(|row| matches!(row, Row::Line { line, .. } if wanted(line.kind)))
                .count()
        };
        let added_start = run_end(start, |kind| matches!(kind, LineKind::Removed { .. }));
        let end = run_end(added_start, |kind| matches!(kind, LineKind::Added { .. }));
        let mut removed = start..added_start;
        let mut added = added_start..end;
        loop {
            match (removed.next(), added.next()) {
                (Some(removed), Some(added)) => items.push(Item::Changed { removed, added }),
                (Some(removed), None) => items.push(Item::Removed(removed)),
                (None, Some(added)) => items.push(Item::Added(added)),
                (None, None) => return end,
            }
        }
    }

    fn index_rows(items: &[Item], row_count: usize) -> Vec<usize> {
        let mut shown: Vec<Option<usize>> = vec![None; row_count];
        for (ix, item) in items.iter().enumerate() {
            shown[item.first_row()] = Some(ix);
            let (left, right) = item.sides();
            for row in [left, right].into_iter().flatten() {
                shown[row] = Some(ix);
            }
        }
        let mut next_shown = items.len();
        let mut item_of_row: Vec<usize> = shown
            .into_iter()
            .rev()
            .map(|ix| {
                next_shown = ix.unwrap_or(next_shown);
                next_shown
            })
            .collect();
        item_of_row.reverse();
        item_of_row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Diff;

    const PATCH: &str = "diff --git a/a.rs b/a.rs\n\
                         --- a/a.rs\n\
                         +++ b/a.rs\n\
                         @@ -1,7 +1,5 @@\n\
                         \x20keep\n\
                         -old 1\n\
                         -old 2\n\
                         -old 3\n\
                         +new 1\n\
                         \x20mid\n\
                         +only added\n\
                         \x20tail\n\
                         -only removed\n";

    fn review() -> Review {
        Review::new("base".to_string(), Diff::parse(PATCH))
    }

    #[test]
    fn side_by_side_pairs_removed_lines_with_following_added_lines() {
        let list = DisplayList::new(&review(), Layout::SideBySide);
        assert_eq!(
            list.items,
            vec![
                Item::Full(0),
                Item::Full(1),
                Item::Context(2),
                Item::Changed {
                    removed: 3,
                    added: 6
                },
                Item::Removed(4),
                Item::Removed(5),
                Item::Context(7),
                Item::Added(8),
                Item::Context(9),
                Item::Removed(10),
            ]
        );
    }

    #[test]
    fn every_row_finds_its_item_when_pairing_reorders_rows() {
        let list = DisplayList::new(&review(), Layout::SideBySide);
        assert_eq!(list.item_of(6), 3);
        assert_eq!(list.item_of(4), 4);
        assert_eq!(list.item_of(7), 6);
    }

    #[test]
    fn inline_shows_every_row_in_order() {
        let review = review();
        let list = DisplayList::new(&review, Layout::Inline);
        let rows: Vec<usize> = (0..list.len())
            .map(|ix| list.item(ix).first_row())
            .collect();
        assert_eq!(rows, (0..review.diff.rows.len()).collect::<Vec<_>>());
    }

    #[test]
    fn collapsed_file_shows_only_its_header_in_both_layouts() {
        let mut review = review();
        review.collapsed.insert(0);
        for layout in [Layout::SideBySide, Layout::Inline] {
            let list = DisplayList::new(&review, layout);
            assert_eq!(list.items, vec![Item::Full(0)]);
            assert_eq!(list.item_of(5), 1);
        }
    }
}
