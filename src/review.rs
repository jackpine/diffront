use crate::diff::{Change, Diff, DiffLine, LineKind, Row};
use std::collections::{BTreeMap, BTreeSet};

/// A diff and the comments left on it, keyed by the row of the commented line.
pub struct Review {
    pub source: String,
    pub diff: Diff,
    pub comments: BTreeMap<usize, String>,
    pub reviewed: BTreeSet<usize>,
    pub collapsed: BTreeSet<usize>,
}

/// How a review's state carries over to a newer diff.
pub struct Carryover {
    comments: BTreeMap<usize, String>,
    reviewed: BTreeSet<usize>,
    collapsed: BTreeSet<usize>,
    pub lost: Vec<LostComment>,
    pub unmarked_files: usize,
}

/// A comment whose line isn't in the newer diff.
pub struct LostComment {
    pub location: String,
    pub code: String,
    pub comment: String,
}

#[derive(Clone, Copy)]
enum Side {
    Old,
    New,
}

impl Side {
    fn of(kind: LineKind) -> Self {
        match kind {
            LineKind::Removed { .. } => Side::Old,
            LineKind::Context { .. } | LineKind::Added { .. } => Side::New,
        }
    }

    fn number(self, kind: LineKind) -> Option<u32> {
        match (self, kind) {
            (Side::Old, LineKind::Context { old, .. } | LineKind::Removed { old }) => Some(old),
            (Side::New, LineKind::Context { new, .. } | LineKind::Added { new }) => Some(new),
            _ => None,
        }
    }
}

/// A line comment with its location resolved.
pub struct Entry<'a> {
    pub row: usize,
    pub location: String,
    pub line: &'a DiffLine,
    pub comment: &'a str,
}

impl Review {
    pub fn new(source: String, diff: Diff) -> Self {
        Self {
            source,
            diff,
            comments: BTreeMap::new(),
            reviewed: BTreeSet::new(),
            collapsed: BTreeSet::new(),
        }
    }

    pub fn set_comment(&mut self, row: usize, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.comments.remove(&row);
        } else {
            self.comments.insert(row, text.to_string());
        }
    }

    pub fn comments_in_file(&self, file: usize) -> usize {
        self.comments
            .keys()
            .filter(|row| self.diff.rows[**row].file() == file)
            .count()
    }

    /// Lists comments in diff order.
    pub fn entries(&self) -> impl Iterator<Item = Entry<'_>> {
        self.comments.iter().filter_map(|(row, comment)| {
            let Row::Line { file, line } = &self.diff.rows[*row] else {
                return None;
            };
            Some(Entry {
                row: *row,
                location: self.location(*file, line.kind),
                line,
                comment,
            })
        })
    }

    fn location(&self, file: usize, kind: LineKind) -> String {
        let file = &self.diff.files[file];
        match kind {
            LineKind::Context { new, .. } | LineKind::Added { new } => {
                format!("{}:{new}", file.path)
            }
            LineKind::Removed { old } => {
                let old_path = match &file.change {
                    Change::Renamed { from } | Change::Copied { from } => from,
                    _ => &file.path,
                };
                format!("{old_path}:{old} (removed line, old numbering)")
            }
        }
    }

    /// Works out which comments and reviewed marks survive a move to `diff`.
    pub fn carry_over(&self, diff: &Diff) -> Carryover {
        let mut comments = BTreeMap::new();
        let mut lost = Vec::new();
        for entry in self.entries() {
            let target = self
                .matching_rows(entry.row, diff)
                .into_iter()
                .find(|row| !comments.contains_key(row));
            match target {
                Some(row) => {
                    comments.insert(row, entry.comment.to_string());
                }
                None => lost.push(LostComment {
                    location: entry.location,
                    code: format!("{}{}", entry.line.kind.marker(), entry.line.text),
                    comment: entry.comment.to_string(),
                }),
            }
        }

        let mut reviewed = BTreeSet::new();
        let mut unmarked_files = 0;
        for file in &self.reviewed {
            let unchanged = diff
                .files
                .iter()
                .position(|candidate| candidate.path == self.diff.files[*file].path)
                .filter(|candidate| {
                    Self::lines_of(&self.diff, *file).eq(Self::lines_of(diff, *candidate))
                });
            match unchanged {
                Some(candidate) => {
                    reviewed.insert(candidate);
                }
                None => unmarked_files += 1,
            }
        }
        // A file that lost its reviewed mark changed, so it opens again to be looked at.
        let collapsed = self
            .collapsed
            .iter()
            .filter_map(|file| {
                let candidate = diff
                    .files
                    .iter()
                    .position(|candidate| candidate.path == self.diff.files[*file].path)?;
                (!self.reviewed.contains(file) || reviewed.contains(&candidate))
                    .then_some(candidate)
            })
            .collect();
        Carryover {
            comments,
            reviewed,
            collapsed,
            lost,
            unmarked_files,
        }
    }

    pub fn apply_update(&mut self, source: String, diff: Diff, carryover: Carryover) {
        self.source = source;
        self.diff = diff;
        self.comments = carryover.comments;
        self.reviewed = carryover.reviewed;
        self.collapsed = carryover.collapsed;
    }

    /// Rows in `diff` showing the same line as `row`, best match first.
    pub fn matching_rows(&self, row: usize, diff: &Diff) -> Vec<usize> {
        let Row::Line { file, line } = &self.diff.rows[row] else {
            return Vec::new();
        };
        let side = Side::of(line.kind);
        let Some(number) = side.number(line.kind) else {
            return Vec::new();
        };
        let mut candidates: Vec<(usize, usize, u32)> = self
            .matching_files(*file, diff)
            .flat_map(|candidate| diff.file_body(candidate))
            .filter_map(|candidate| {
                let Row::Line { line: other, .. } = &diff.rows[candidate] else {
                    return None;
                };
                let other_number = side.number(other.kind)?;
                (other.text == line.text).then(|| {
                    let neighbors = Self::shared_neighbors(&self.diff, row, diff, candidate);
                    (candidate, neighbors, other_number.abs_diff(number))
                })
            })
            .collect();

        // Repeated lines like `}` need nearby evidence, or a comment could jump to the wrong one.
        if candidates.len() > 1 || line.text.trim().is_empty() {
            candidates.retain(|(_, neighbors, _)| *neighbors > 0);
        }
        candidates
            .sort_by_key(|(_, neighbors, distance)| (std::cmp::Reverse(*neighbors), *distance));
        candidates.into_iter().map(|(row, ..)| row).collect()
    }

    fn matching_files<'a>(
        &'a self,
        file: usize,
        diff: &'a Diff,
    ) -> impl Iterator<Item = usize> + 'a {
        let old = &self.diff.files[file];
        let old_from = Self::renamed_from(&old.change);
        diff.files.iter().enumerate().filter_map(move |(ix, new)| {
            let new_from = Self::renamed_from(&new.change);
            (new.path == old.path || old_from == Some(&new.path) || new_from == Some(&old.path))
                .then_some(ix)
        })
    }

    fn renamed_from(change: &Change) -> Option<&String> {
        match change {
            Change::Renamed { from } | Change::Copied { from } => Some(from),
            _ => None,
        }
    }

    fn shared_neighbors(old: &Diff, old_row: usize, new: &Diff, new_row: usize) -> usize {
        let text = |diff: &'_ Diff, row: Option<usize>| -> Option<String> {
            match diff.rows.get(row?) {
                Some(Row::Line { line, .. }) => Some(line.text.clone()),
                _ => None,
            }
        };
        let before = text(old, old_row.checked_sub(1));
        let after = text(old, Some(old_row + 1));
        let same_before = before.is_some() && before == text(new, new_row.checked_sub(1));
        let same_after = after.is_some() && after == text(new, Some(new_row + 1));
        usize::from(same_before) + usize::from(same_after)
    }

    fn lines_of(diff: &Diff, file: usize) -> impl Iterator<Item = &DiffLine> {
        diff.rows[diff.file_body(file)]
            .iter()
            .filter_map(|row| match row {
                Row::Line { line, .. } => Some(line),
                _ => None,
            })
    }

    /// Renders reviews as Markdown for pasting into an agent; `overall` goes with the first.
    pub fn export_all(overall: &str, reviews: &[&Review]) -> String {
        let mut out = String::new();
        for (ix, review) in reviews.iter().enumerate() {
            let has_overall = ix == 0 && !overall.is_empty();
            if !has_overall && review.comments.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out += &format!("Review comments on `{}`:\n", review.source);
            if has_overall {
                out += &format!("\n## Overall\n{overall}\n");
            }
            review.export_entries(&mut out);
        }
        out
    }

    fn export_entries(&self, out: &mut String) {
        for entry in self.entries() {
            let marker = entry.line.kind.marker();
            let fence = if entry.line.text.contains("```") {
                "````"
            } else {
                "```"
            };
            *out += &format!(
                "\n## {}\n{fence}diff\n{marker}{}\n{fence}\n{}\n",
                entry.location, entry.line.text, entry.comment
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review() -> Review {
        Review::new(
            "git diff HEAD".to_string(),
            Diff::parse(
                "diff --git a/old.rs b/new.rs\n\
                 rename from old.rs\n\
                 rename to new.rs\n\
                 --- a/old.rs\n\
                 +++ b/new.rs\n\
                 @@ -7,2 +7,2 @@\n\
                 \x20same\n\
                 -let a = \"```\";\n\
                 +let b = 2;\n",
            ),
        )
    }

    fn row_of(review: &Review, wanted: &str) -> usize {
        review
            .diff
            .rows
            .iter()
            .position(|row| matches!(row, Row::Line { line, .. } if line.text == wanted))
            .unwrap()
    }

    #[test]
    fn exports_comments_in_diff_order() {
        let mut review = review();
        let added = row_of(&review, "let b = 2;");
        let removed = row_of(&review, "let a = \"```\";");
        let context = row_of(&review, "same");
        review.set_comment(added, "rename b");
        review.set_comment(removed, "why drop this?");
        review.set_comment(context, "  tidy\nthis up  ");
        assert_eq!(
            Review::export_all("", &[&review]),
            "Review comments on `git diff HEAD`:\n\
             \n## new.rs:7\n```diff\n same\n```\ntidy\nthis up\n\
             \n## old.rs:8 (removed line, old numbering)\n````diff\n-let a = \"```\";\n````\nwhy drop this?\n\
             \n## new.rs:8\n```diff\n+let b = 2;\n```\nrename b\n"
        );
    }

    #[test]
    fn exports_overall_comment_before_line_comments() {
        let mut review = review();
        let row = row_of(&review, "same");
        review.set_comment(row, "note");
        assert_eq!(
            Review::export_all("Looks good overall.", &[&review]),
            "Review comments on `git diff HEAD`:\n\
             \n## Overall\nLooks good overall.\n\
             \n## new.rs:7\n```diff\n same\n```\nnote\n"
        );
    }

    #[test]
    fn exports_each_commented_review_under_its_own_heading() {
        let mut files = review();
        let row = row_of(&files, "same");
        files.set_comment(row, "on the whole diff");
        let mut commit = review();
        commit.source = "commit abc123 (Fix it)".to_string();
        commit.set_comment(row, "on one commit");
        let mut untouched = review();
        untouched.source = "commit def456 (Other)".to_string();
        assert_eq!(
            Review::export_all("", &[&files, &commit, &untouched]),
            "Review comments on `git diff HEAD`:\n\
             \n## new.rs:7\n```diff\n same\n```\non the whole diff\n\
             \nReview comments on `commit abc123 (Fix it)`:\n\
             \n## new.rs:7\n```diff\n same\n```\non one commit\n"
        );
    }

    #[test]
    fn blank_comment_removes_existing() {
        let mut review = review();
        let row = row_of(&review, "same");
        review.set_comment(row, "note");
        assert_eq!(review.comments_in_file(0), 1);
        review.set_comment(row, "  \n ");
        assert_eq!(review.comments_in_file(0), 0);
    }

    const BASE: &str = "diff --git a/a.rs b/a.rs\n\
                        --- a/a.rs\n\
                        +++ b/a.rs\n\
                        @@ -1,4 +1,4 @@\n\
                        \x20start\n\
                        -old\n\
                        +new\n\
                        \x20}\n\
                        \x20end\n";

    fn commented_on(patch: &str, wanted: &str) -> Review {
        let mut review = Review::new("base".to_string(), Diff::parse(patch));
        let row = row_of(&review, wanted);
        review.set_comment(row, "note");
        review
    }

    fn commented_lines<'a>(carried: &Carryover, diff: &'a Diff) -> Vec<&'a str> {
        carried
            .comments
            .keys()
            .map(|row| match &diff.rows[*row] {
                Row::Line { line, .. } => line.text.as_str(),
                _ => panic!("comment on a non-line row"),
            })
            .collect()
    }

    #[test]
    fn comment_follows_its_line_when_the_line_moves() {
        let review = commented_on(BASE, "new");
        let moved = Diff::parse(
            "diff --git a/a.rs b/a.rs\n\
             --- a/a.rs\n\
             +++ b/a.rs\n\
             @@ -10,4 +30,4 @@\n\
             \x20start\n\
             -old\n\
             +new\n\
             \x20}\n\
             \x20end\n",
        );
        let carried = review.carry_over(&moved);
        assert!(carried.lost.is_empty());
        assert_eq!(commented_lines(&carried, &moved), vec!["new"]);
    }

    #[test]
    fn added_line_comment_survives_becoming_context() {
        let review = commented_on(BASE, "new");
        let later_base = Diff::parse(
            "diff --git a/a.rs b/a.rs\n\
             --- a/a.rs\n\
             +++ b/a.rs\n\
             @@ -1,2 +1,3 @@\n\
             \x20new\n\
             +extra\n\
             \x20}\n",
        );
        let carried = review.carry_over(&later_base);
        assert!(carried.lost.is_empty());
        assert_eq!(commented_lines(&carried, &later_base), vec!["new"]);
    }

    #[test]
    fn comment_is_lost_when_its_line_is_gone() {
        let review = commented_on(BASE, "new");
        let gone = Diff::parse(
            "diff --git a/a.rs b/a.rs\n\
             --- a/a.rs\n\
             +++ b/a.rs\n\
             @@ -1,2 +1,2 @@\n\
             \x20start\n\
             -old\n\
             +rewritten\n",
        );
        let carried = review.carry_over(&gone);
        assert!(carried.comments.is_empty());
        assert_eq!(carried.lost[0].location, "a.rs:2");
        assert_eq!(carried.lost[0].code, "+new");
        assert_eq!(carried.lost[0].comment, "note");
    }

    #[test]
    fn removed_line_comment_does_not_match_the_new_side() {
        let review = commented_on(BASE, "old");
        let reintroduced = Diff::parse(
            "diff --git a/a.rs b/a.rs\n\
             --- a/a.rs\n\
             +++ b/a.rs\n\
             @@ -1 +1,2 @@\n\
             \x20start\n\
             +old\n",
        );
        assert_eq!(review.carry_over(&reintroduced).lost.len(), 1);
    }

    #[test]
    fn repeated_line_without_matching_neighbors_is_lost_not_guessed() {
        let review = commented_on(BASE, "}");
        let elsewhere = Diff::parse(
            "diff --git a/a.rs b/a.rs\n\
             --- a/a.rs\n\
             +++ b/a.rs\n\
             @@ -1,4 +1,4 @@\n\
             \x20one\n\
             \x20}\n\
             \x20two\n\
             \x20}\n",
        );
        assert_eq!(review.carry_over(&elsewhere).lost.len(), 1);
    }

    #[test]
    fn repeated_line_uses_its_neighbors_to_pick_the_right_one() {
        let review = commented_on(BASE, "}");
        let shifted = Diff::parse(
            "diff --git a/a.rs b/a.rs\n\
             --- a/a.rs\n\
             +++ b/a.rs\n\
             @@ -1,5 +1,5 @@\n\
             \x20one\n\
             \x20}\n\
             \x20new\n\
             \x20}\n\
             \x20end\n",
        );
        let carried = review.carry_over(&shifted);
        assert!(carried.lost.is_empty());
        let expected = shifted
            .rows
            .iter()
            .rposition(|row| matches!(row, Row::Line { line, .. } if line.text == "}"));
        assert_eq!(carried.comments.keys().next().copied(), expected);
    }

    #[test]
    fn comment_follows_a_rename() {
        let review = commented_on(BASE, "new");
        let renamed = Diff::parse(
            "diff --git a/a.rs b/b.rs\n\
             rename from a.rs\n\
             rename to b.rs\n\
             --- a/a.rs\n\
             +++ b/b.rs\n\
             @@ -1,4 +1,4 @@\n\
             \x20start\n\
             -old\n\
             +new\n\
             \x20}\n\
             \x20end\n",
        );
        assert!(review.carry_over(&renamed).lost.is_empty());
    }

    #[test]
    fn reviewed_mark_survives_only_if_the_file_is_unchanged() {
        let mut review = Review::new("base".to_string(), Diff::parse(BASE));
        review.reviewed.insert(0);

        let same = review.carry_over(&Diff::parse(BASE));
        assert_eq!(same.reviewed, BTreeSet::from([0]));
        assert_eq!(same.unmarked_files, 0);

        let changed = review.carry_over(&Diff::parse(&BASE.replace("+new", "+newer")));
        assert!(changed.reviewed.is_empty());
        assert_eq!(changed.unmarked_files, 1);
    }

    #[test]
    fn changed_file_reopens_only_if_it_lost_a_reviewed_mark() {
        let changed = Diff::parse(&BASE.replace("+new", "+newer"));

        let mut merely_collapsed = Review::new("base".to_string(), Diff::parse(BASE));
        merely_collapsed.collapsed.insert(0);
        assert_eq!(
            merely_collapsed.carry_over(&changed).collapsed,
            BTreeSet::from([0])
        );

        let mut reviewed = Review::new("base".to_string(), Diff::parse(BASE));
        reviewed.collapsed.insert(0);
        reviewed.reviewed.insert(0);
        assert!(reviewed.carry_over(&changed).collapsed.is_empty());
    }
}
