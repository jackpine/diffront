use crate::diff::{Change, Diff, DiffLine, LineKind, Row};
use std::collections::BTreeMap;

/// A diff and the comments left on it, keyed by the row of the commented line.
pub struct Review {
    pub source: String,
    pub diff: Diff,
    pub comments: BTreeMap<usize, String>,
    pub overall: String,
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
            overall: String::new(),
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

    pub fn set_overall(&mut self, text: &str) {
        self.overall = text.trim().to_string();
    }

    pub fn comments_in_file(&self, file: usize) -> usize {
        self.comments
            .keys()
            .filter(|row| self.diff.rows[**row].file() == file)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.overall.is_empty() && self.comments.is_empty()
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

    /// Renders the review as Markdown for pasting into an agent.
    pub fn export(&self) -> String {
        let mut out = format!("Review comments on `{}`:\n", self.source);
        if !self.overall.is_empty() {
            out += &format!("\n## Overall\n{}\n", self.overall);
        }
        for entry in self.entries() {
            let marker = match entry.line.kind {
                LineKind::Context { .. } => ' ',
                LineKind::Added { .. } => '+',
                LineKind::Removed { .. } => '-',
            };
            let fence = if entry.line.text.contains("```") {
                "````"
            } else {
                "```"
            };
            out += &format!(
                "\n## {}\n{fence}diff\n{marker}{}\n{fence}\n{}\n",
                entry.location, entry.line.text, entry.comment
            );
        }
        out
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
            review.export(),
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
        review.set_overall("  Looks good overall.  ");
        assert_eq!(
            review.export(),
            "Review comments on `git diff HEAD`:\n\
             \n## Overall\nLooks good overall.\n\
             \n## new.rs:7\n```diff\n same\n```\nnote\n"
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
}
