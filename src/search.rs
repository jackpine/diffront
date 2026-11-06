use crate::diff::Diff;
use crate::display::DisplayList;
use std::collections::HashMap;
use std::ops::Range;

/// A hit in a row's rendered text.
#[derive(Clone, Debug, PartialEq)]
struct SearchMatch {
    row: usize,
    range: Range<usize>,
}

/// A query and its matches in the rendered diff, in the order they appear on screen.
#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub case_sensitive: bool,
    query: String,
    matches: Vec<SearchMatch>,
    matches_by_row: HashMap<usize, Range<usize>>,
    current: Option<usize>,
}

impl Search {
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Runs `query`, selecting the first match at or after the current match or `top_row`.
    pub fn run(
        &mut self,
        query: String,
        diff: &Diff,
        display: &DisplayList,
        top_row: Option<usize>,
    ) {
        let anchor = self.current_row().or(top_row).unwrap_or(0);
        self.query = query;
        self.collect_matches(diff, display);
        self.select_from(anchor, display);
    }

    /// Re-runs the query against a changed display, keeping the current match if it remains.
    pub fn refresh(&mut self, diff: &Diff, display: &DisplayList) {
        if !self.open {
            return;
        }
        let previous = self.current.map(|ix| self.matches[ix].clone());
        self.collect_matches(diff, display);
        let Some(previous) = previous else {
            self.select_from(0, display);
            return;
        };
        match self.matches.iter().position(|hit| *hit == previous) {
            Some(ix) => self.current = Some(ix),
            None => self.select_from(previous.row, display),
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.matches_by_row.clear();
        self.current = None;
    }

    /// Moves `delta` matches along, wrapping at the ends.
    pub fn step(&mut self, delta: isize) {
        if let Some(current) = self.current {
            let target = current as isize + delta;
            self.current = Some(target.rem_euclid(self.matches.len() as isize) as usize);
        }
    }

    pub fn current_row(&self) -> Option<usize> {
        self.current.map(|ix| self.matches[ix].row)
    }

    /// The current match's 1-based position, and the number of matches.
    pub fn position(&self) -> Option<(usize, usize)> {
        self.current.map(|ix| (ix + 1, self.matches.len()))
    }

    /// The matches in a row's rendered text, each flagged if it's the current one.
    pub fn matches_in(&self, row: usize) -> impl Iterator<Item = (Range<usize>, bool)> + '_ {
        let span = self.matches_by_row.get(&row).cloned().unwrap_or(0..0);
        span.map(|ix| (self.matches[ix].range.clone(), self.current == Some(ix)))
    }

    fn collect_matches(&mut self, diff: &Diff, display: &DisplayList) {
        self.matches.clear();
        self.matches_by_row.clear();
        let needle = self.folded_needle();
        if needle.is_empty() {
            return;
        }
        for row in (0..display.len()).flat_map(|ix| display.item(ix).rows()) {
            let first = self.matches.len();
            let text = diff.row_text(row);
            let ranges: Vec<_> = self.ranges_in(&text, &needle).collect();
            self.matches
                .extend(ranges.into_iter().map(|range| SearchMatch { row, range }));
            if self.matches.len() > first {
                self.matches_by_row.insert(row, first..self.matches.len());
            }
        }
    }

    fn select_from(&mut self, row: usize, display: &DisplayList) {
        if self.matches.is_empty() {
            self.current = None;
            return;
        }
        let anchor = display.item_of(row.min(display.row_count() - 1));
        let next = self
            .matches
            .partition_point(|hit| display.item_of(hit.row) < anchor);
        self.current = Some(if next == self.matches.len() { 0 } else { next });
    }

    fn folded_needle(&self) -> Vec<char> {
        self.query.chars().flat_map(|ch| self.fold(ch)).collect()
    }

    fn fold(&self, ch: char) -> Vec<char> {
        if self.case_sensitive {
            vec![ch]
        } else {
            ch.to_lowercase().collect()
        }
    }

    /// The non-overlapping occurrences of `needle` in `text`.
    fn ranges_in<'a>(
        &'a self,
        text: &'a str,
        needle: &'a [char],
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        let mut searched_to = 0;
        text.char_indices().filter_map(move |(start, _)| {
            if start < searched_to {
                return None;
            }
            let len = self.match_len(&text[start..], needle)?;
            searched_to = start + len;
            Some(start..searched_to)
        })
    }

    /// The bytes of `text` that `needle` matches from its start.
    fn match_len(&self, text: &str, needle: &[char]) -> Option<usize> {
        let mut remaining = needle;
        for (offset, ch) in text.char_indices() {
            if remaining.is_empty() {
                return Some(offset);
            }
            remaining = remaining.strip_prefix(self.fold(ch).as_slice())?;
        }
        remaining.is_empty().then_some(text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::Layout;
    use crate::review::Review;

    const PATCH: &str = "diff --git a/one.rs b/one.rs\n\
                         @@ -1,3 +1,3 @@\n\
                         \x20same\n\
                         -old value\n\
                         -old again\n\
                         +new value\n\
                         +new again\n\
                         diff --git a/two.rs b/two.rs\n\
                         @@ -1 +1 @@\n\
                         -gone\n\
                         +Value\n";
    const SAME: usize = 2;

    fn review() -> Review {
        Review::new(String::new(), Diff::parse(PATCH))
    }

    fn search_in(query: &str, review: &Review, layout: Layout) -> (Search, DisplayList) {
        let display = DisplayList::new(review, layout);
        let mut search = Search {
            open: true,
            ..Search::default()
        };
        search.run(query.to_string(), &review.diff, &display, None);
        (search, display)
    }

    fn rows(search: &Search) -> Vec<usize> {
        search.matches.iter().map(|hit| hit.row).collect()
    }

    #[test]
    fn match_order_follows_the_screen_in_each_layout() {
        let review = review();
        let (search, _) = search_in("n", &review, Layout::SideBySide);
        assert_eq!(rows(&search), [0, 5, 4, 6, 6, 9]);
        let (search, _) = search_in("n", &review, Layout::Inline);
        assert_eq!(rows(&search), [0, 4, 5, 6, 6, 9]);
    }

    #[test]
    fn a_context_line_counts_once_though_it_shows_twice() {
        let review = review();
        let (search, _) = search_in("same", &review, Layout::SideBySide);
        assert_eq!(rows(&search), [SAME]);
    }

    #[test]
    fn file_paths_and_hunk_headers_are_searched() {
        let review = review();
        let (search, _) = search_in("two.rs", &review, Layout::SideBySide);
        assert_eq!(search.position(), Some((1, 1)));
        let (search, _) = search_in("@@ -1,3", &review, Layout::SideBySide);
        assert_eq!(search.position(), Some((1, 1)));
    }

    #[test]
    fn collapsed_files_only_search_their_header() {
        let mut review = review();
        review.collapsed.insert(0);
        let (search, _) = search_in("again", &review, Layout::SideBySide);
        assert_eq!(search.position(), None);
        let (search, _) = search_in("one.rs", &review, Layout::SideBySide);
        assert_eq!(search.position(), Some((1, 1)));
    }

    #[test]
    fn case_only_matters_when_asked() {
        let review = review();
        let display = DisplayList::new(&review, Layout::SideBySide);
        let mut search = Search::default();
        search.run("VALUE".to_string(), &review.diff, &display, None);
        assert_eq!(search.position(), Some((1, 3)));
        search.case_sensitive = true;
        search.run("Value".to_string(), &review.diff, &display, None);
        assert_eq!(search.position(), Some((1, 1)));
    }

    #[test]
    fn ranges_are_byte_offsets_and_do_not_overlap() {
        let search = Search::default();
        let needle = ['a', 'a'];
        let ranges: Vec<_> = search.ranges_in("åAaaa", &needle).collect();
        assert_eq!(ranges, [2..4, 4..6]);
    }

    #[test]
    fn a_character_folding_to_several_must_match_whole() {
        let search = Search::default();
        assert_eq!(search.match_len("İx", &['i']), None);
    }

    #[test]
    fn stepping_wraps_both_ways() {
        let review = review();
        let (mut search, _) = search_in("value", &review, Layout::SideBySide);
        search.step(-1);
        assert_eq!(search.position(), Some((3, 3)));
        search.step(1);
        assert_eq!(search.position(), Some((1, 3)));
    }

    #[test]
    fn first_match_comes_from_the_scroll_position_and_wraps() {
        let review = review();
        let display = DisplayList::new(&review, Layout::SideBySide);
        let mut search = Search::default();
        search.run("again".to_string(), &review.diff, &display, Some(10));
        assert_eq!(search.position(), Some((1, 2)));
        let mut search = Search::default();
        search.run("value".to_string(), &review.diff, &display, Some(9));
        assert_eq!(search.current_row(), Some(10));
    }

    #[test]
    fn refining_a_query_stays_near_the_current_match() {
        let review = review();
        let display = DisplayList::new(&review, Layout::SideBySide);
        let mut search = Search::default();
        search.run("value".to_string(), &review.diff, &display, Some(9));
        search.run("valu".to_string(), &review.diff, &display, Some(0));
        assert_eq!(search.current_row(), Some(10));
    }

    #[test]
    fn refresh_keeps_the_current_match_across_a_collapse() {
        let mut review = review();
        let (mut search, _) = search_in("value", &review, Layout::SideBySide);
        search.step(-1);
        review.collapsed.insert(0);
        let display = DisplayList::new(&review, Layout::SideBySide);
        search.refresh(&review.diff, &display);
        assert_eq!(search.position(), Some((1, 1)));
        assert_eq!(search.current_row(), Some(10));
    }

    #[test]
    fn refresh_moves_on_when_the_current_match_is_hidden() {
        let mut review = review();
        let (mut search, _) = search_in("value", &review, Layout::SideBySide);
        review.collapsed.insert(0);
        let display = DisplayList::new(&review, Layout::SideBySide);
        search.refresh(&review.diff, &display);
        assert_eq!(search.current_row(), Some(10));
        search.close();
        assert_eq!(search.matches_in(10).count(), 0);
    }

    #[test]
    fn refresh_does_nothing_when_closed() {
        let review = review();
        let display = DisplayList::new(&review, Layout::SideBySide);
        let mut search = Search {
            query: "value".to_string(),
            ..Search::default()
        };
        search.refresh(&review.diff, &display);
        assert_eq!(search.position(), None);
    }

    #[test]
    fn current_match_is_flagged_within_its_row() {
        let review = review();
        let (search, _) = search_in("a", &review, Layout::Inline);
        let flagged: Vec<_> = search.matches_in(SAME).collect();
        assert_eq!(flagged, [(1..2, true)]);
    }
}
