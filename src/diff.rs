use std::borrow::Cow;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, PartialEq)]
pub enum Change {
    Modified,
    Added,
    Deleted,
    Renamed { from: String },
    Copied { from: String },
}

#[derive(Debug, PartialEq)]
pub enum Content {
    Text,
    Binary,
    Combined,
}

#[derive(Debug, PartialEq)]
pub struct FileHeader {
    pub path: String,
    pub change: Change,
    pub content: Content,
    pub added: usize,
    pub removed: usize,
    pub first_row: usize,
    /// The pre-image's blob, which hidden context lines are read from.
    pub old_blob: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineKind {
    Context { old: u32, new: u32 },
    Added { new: u32 },
    Removed { old: u32 },
}

impl LineKind {
    pub fn marker(self) -> char {
        match self {
            LineKind::Context { .. } => ' ',
            LineKind::Added { .. } => '+',
            LineKind::Removed { .. } => '-',
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    /// Byte ranges of `text` that differ from the line it's paired with.
    pub emphasis: Vec<Range<usize>>,
}

impl DiffLine {
    const TAB: &str = "    ";
    /// Token comparisons beyond this are too slow to be worth it.
    const MAX_WORD_DIFF_CELLS: usize = 250_000;

    /// The text as drawn, with tabs expanded.
    pub fn display_text(&self) -> String {
        self.text.replace('\t', Self::TAB)
    }

    /// Converts an offset in `text` to one in `display_text`.
    pub fn display_offset(&self, offset: usize) -> usize {
        let tabs = self.text[..offset].matches('\t').count();
        offset + tabs * (Self::TAB.len() - 1)
    }

    /// Converts an offset in `display_text` to one in `text`, snapping to the start of a tab.
    pub fn source_offset(&self, display_offset: usize) -> usize {
        let mut drawn = 0;
        for (offset, ch) in self.text.char_indices() {
            drawn += if ch == '\t' {
                Self::TAB.len()
            } else {
                ch.len_utf8()
            };
            if drawn > display_offset {
                return offset;
            }
        }
        self.text.len()
    }

    /// Marks the words that differ between `self` and `other`, unless the lines are too dissimilar.
    fn emphasize_against(&mut self, other: &mut DiffLine) {
        let old: Vec<_> = self.text.split_word_bound_indices().collect();
        let new: Vec<_> = other.text.split_word_bound_indices().collect();
        let same = |(a, b): (&(usize, &str), &(usize, &str))| a.1 == b.1;
        let prefix = old.iter().zip(&new).take_while(|pair| same(*pair)).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|pair| same(*pair))
            .count();
        let old_mid = &old[prefix..old.len() - suffix];
        let new_mid = &new[prefix..new.len() - suffix];
        if old_mid.len() * new_mid.len() > Self::MAX_WORD_DIFF_CELLS {
            return;
        }
        let mut lcs = vec![vec![0u32; new_mid.len() + 1]; old_mid.len() + 1];
        for i in (0..old_mid.len()).rev() {
            for j in (0..new_mid.len()).rev() {
                lcs[i][j] = if old_mid[i].1 == new_mid[j].1 {
                    lcs[i + 1][j + 1] + 1
                } else {
                    lcs[i + 1][j].max(lcs[i][j + 1])
                };
            }
        }
        let mut old_kept = vec![true; old.len()];
        let mut new_kept = vec![true; new.len()];
        old_kept[prefix..old.len() - suffix].fill(false);
        new_kept[prefix..new.len() - suffix].fill(false);
        let (mut i, mut j) = (0, 0);
        while i < old_mid.len() && j < new_mid.len() {
            if old_mid[i].1 == new_mid[j].1 {
                old_kept[prefix + i] = true;
                new_kept[prefix + j] = true;
                i += 1;
                j += 1;
            } else if lcs[i + 1][j] >= lcs[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }

        let visible_len = |tokens: &[(usize, &str)], kept: Option<&[bool]>| -> usize {
            tokens
                .iter()
                .enumerate()
                .filter(|(ix, _)| kept.is_none_or(|kept| kept[*ix]))
                .map(|(_, (_, token))| token.trim().len())
                .sum()
        };
        let common = visible_len(&old, Some(&old_kept));
        if common * 4 < visible_len(&old, None) + visible_len(&new, None) {
            return;
        }
        self.emphasis = Self::changed_ranges(&old, &old_kept);
        other.emphasis = Self::changed_ranges(&new, &new_kept);
    }

    /// Merges runs of changed tokens, bridging whitespace between them.
    fn changed_ranges(tokens: &[(usize, &str)], kept: &[bool]) -> Vec<Range<usize>> {
        let mut ranges: Vec<Range<usize>> = Vec::new();
        let mut bridge_from = None;
        for (&(start, token), &kept) in tokens.iter().zip(kept) {
            let end = start + token.len();
            if !kept {
                match (ranges.last_mut(), bridge_from) {
                    (Some(last), Some(bridge)) if last.end == bridge => last.end = end,
                    (Some(last), None) if last.end == start => last.end = end,
                    _ => ranges.push(start..end),
                }
                bridge_from = None;
            } else if token.trim().is_empty() {
                bridge_from = bridge_from.or(Some(start));
            } else {
                bridge_from = None;
            }
        }
        ranges
    }
}

/// A hunk header, with the lines it spans on each side.
#[derive(Debug, PartialEq)]
pub struct Hunk {
    pub file: usize,
    pub old: Range<u32>,
    pub new: Range<u32>,
    /// What git names after the ranges, such as the enclosing function.
    section: String,
}

impl Hunk {
    fn parse(file: usize, header: &str) -> Option<Self> {
        let ranges = header.strip_prefix("@@ -")?;
        let (old, ranges) = ranges.split_once(" +")?;
        let (new, section) = ranges.split_once(" @@")?;
        Some(Self {
            file,
            old: Self::parse_range(old)?,
            new: Self::parse_range(new)?,
            section: section.to_string(),
        })
    }

    /// Reads `start[,count]`, where an empty range's start is the line before it.
    fn parse_range(range: &str) -> Option<Range<u32>> {
        let (start, count) = match range.split_once(',') {
            Some((start, count)) => (start.parse().ok()?, count.parse().ok()?),
            None => (range.parse().ok()?, 1),
        };
        let start = if count == 0 { start + 1 } else { start };
        Some(start..start + count)
    }

    fn format_range(range: &Range<u32>) -> String {
        let start = if range.is_empty() {
            range.start - 1
        } else {
            range.start
        };
        match range.len() {
            1 => start.to_string(),
            count => format!("{start},{count}"),
        }
    }

    pub fn header(&self) -> String {
        format!(
            "@@ -{} +{} @@{}",
            Self::format_range(&self.old),
            Self::format_range(&self.new),
            self.section
        )
    }
}

/// One entry in the flattened, scrollable diff.
#[derive(Debug, PartialEq)]
pub enum Row {
    File(usize),
    Hunk(Hunk),
    Line { file: usize, line: DiffLine },
}

impl Row {
    pub fn file(&self) -> usize {
        match self {
            Row::File(file) | Row::Hunk(Hunk { file, .. }) | Row::Line { file, .. } => *file,
        }
    }
}

/// Parsed `git diff` output.
#[derive(Debug, Default, PartialEq)]
pub struct Diff {
    pub files: Vec<FileHeader>,
    pub rows: Vec<Row>,
}

/// Position within a hunk; its line counts tell content apart from headers like `--- a/file`.
struct HunkCursor {
    old: u32,
    new: u32,
    old_remaining: u32,
    new_remaining: u32,
}

impl HunkCursor {
    fn new(hunk: &Hunk) -> Self {
        Self {
            old: hunk.old.start,
            new: hunk.new.start,
            old_remaining: hunk.old.len() as u32,
            new_remaining: hunk.new.len() as u32,
        }
    }

    fn is_done(&self) -> bool {
        self.old_remaining == 0 && self.new_remaining == 0
    }

    fn next_kind(&mut self, marker: Option<char>) -> LineKind {
        match marker {
            Some('+') => {
                let new = self.new;
                self.new += 1;
                self.new_remaining = self.new_remaining.saturating_sub(1);
                LineKind::Added { new }
            }
            Some('-') => {
                let old = self.old;
                self.old += 1;
                self.old_remaining = self.old_remaining.saturating_sub(1);
                LineKind::Removed { old }
            }
            _ => {
                let (old, new) = (self.old, self.new);
                self.old += 1;
                self.new += 1;
                self.old_remaining = self.old_remaining.saturating_sub(1);
                self.new_remaining = self.new_remaining.saturating_sub(1);
                LineKind::Context { old, new }
            }
        }
    }
}

impl Diff {
    /// The rows of a file below its header.
    pub fn file_body(&self, file: usize) -> Range<usize> {
        let end = self
            .files
            .get(file + 1)
            .map_or(self.rows.len(), |next| next.first_row);
        self.files[file].first_row + 1..end
    }

    /// The text a row draws.
    pub fn row_text(&self, row: usize) -> Cow<'_, str> {
        match &self.rows[row] {
            Row::File(file) => Cow::Borrowed(&self.files[*file].path),
            Row::Hunk(hunk) => Cow::Owned(hunk.header()),
            Row::Line { line, .. } => Cow::Owned(line.display_text()),
        }
    }

    /// The old file's lines left out between the hunk at `row` and what precedes it in its file.
    pub fn gap_above(&self, row: usize) -> Range<u32> {
        let previous_end = self
            .previous_hunk(row)
            .map_or(1, |previous| self.hunk(previous).old.end);
        previous_end..self.hunk(row).old.start
    }

    fn previous_hunk(&self, row: usize) -> Option<usize> {
        let file_start = self.files[self.rows[row].file()].first_row;
        (file_start..row)
            .rev()
            .find(|ix| matches!(self.rows[*ix], Row::Hunk(_)))
    }

    fn hunk(&self, row: usize) -> &Hunk {
        let Row::Hunk(hunk) = &self.rows[row] else {
            unreachable!("row {row} is not a hunk header");
        };
        hunk
    }

    fn hunk_mut(&mut self, row: usize) -> &mut Hunk {
        let Row::Hunk(hunk) = &mut self.rows[row] else {
            unreachable!("row {row} is not a hunk header");
        };
        hunk
    }

    /// Shows up to `limit` of the old file's lines hidden above the hunk at `row`, merging it into
    /// the hunk before once none are left. Returns how many rows replace the header.
    pub fn expand_above(&mut self, row: usize, old_lines: &[String], limit: u32) -> usize {
        let previous = self.previous_hunk(row);
        let gap = self.gap_above(row);
        let revealed = gap.end.saturating_sub(limit).max(gap.start)..gap.end;
        let hunk = self.hunk_mut(row);
        hunk.old.start = revealed.start;
        hunk.new.start -= revealed.len() as u32;
        let file = hunk.file;
        let (old_end, new_end) = (hunk.old.end, hunk.new.end);
        let lines: Vec<Row> = revealed
            .clone()
            .zip(hunk.new.start..)
            .map(|(old, new)| Row::Line {
                file,
                line: DiffLine {
                    kind: LineKind::Context { old, new },
                    text: old_lines[old as usize - 1].clone(),
                    emphasis: Vec::new(),
                },
            })
            .collect();
        let keeps_header = revealed.start > gap.start;
        if !keeps_header && let Some(previous) = previous {
            let previous = self.hunk_mut(previous);
            previous.old.end = old_end;
            previous.new.end = new_end;
        }
        let replacement = usize::from(keeps_header) + lines.len();
        self.rows
            .splice(row + usize::from(keeps_header)..row + 1, lines);
        for later in &mut self.files[file + 1..] {
            later.first_row = later.first_row + replacement - 1;
        }
        replacement
    }

    pub fn parse(patch: &str) -> Self {
        let mut diff = Self::default();
        let mut hunk: Option<HunkCursor> = None;
        for text in patch.lines() {
            if let Some(cursor) = &mut hunk {
                if !text.starts_with('\\') {
                    diff.push_line(cursor, text);
                }
                if cursor.is_done() {
                    hunk = None;
                }
            } else if let Some(paths) = text.strip_prefix("diff --git ") {
                diff.start_file(Self::path_from_git_header(paths), Content::Text);
            } else if let Some(path) = text
                .strip_prefix("diff --cc ")
                .or(text.strip_prefix("diff --combined "))
            {
                diff.start_file(Self::unquote(path), Content::Combined);
            } else if !diff.files.is_empty() {
                hunk = diff.parse_header_line(text);
            }
        }
        diff.emphasize_changed_words();
        diff
    }

    /// Marks the words that differ between each removed line and the added line paired with it.
    fn emphasize_changed_words(&mut self) {
        let mut row = 0;
        while row < self.rows.len() {
            let removed = self.line_run(row, |kind| matches!(kind, LineKind::Removed { .. }));
            let added = self.line_run(removed.end, |kind| matches!(kind, LineKind::Added { .. }));
            for (old, new) in removed.clone().zip(added.clone()) {
                let (head, tail) = self.rows.split_at_mut(new);
                let (Row::Line { line: old_line, .. }, Row::Line { line: new_line, .. }) =
                    (&mut head[old], &mut tail[0])
                else {
                    unreachable!("runs contain only line rows");
                };
                old_line.emphasize_against(new_line);
            }
            row = added.end.max(row + 1);
        }
    }

    /// The consecutive line rows from `start` whose kind is `wanted`.
    pub fn line_run(&self, start: usize, wanted: fn(LineKind) -> bool) -> Range<usize> {
        let len = self.rows[start..]
            .iter()
            .take_while(|row| matches!(row, Row::Line { line, .. } if wanted(line.kind)))
            .count();
        start..start + len
    }

    fn start_file(&mut self, path: String, content: Content) {
        self.rows.push(Row::File(self.files.len()));
        self.files.push(FileHeader {
            path,
            change: Change::Modified,
            content,
            added: 0,
            removed: 0,
            first_row: self.rows.len() - 1,
            old_blob: None,
        });
    }

    fn parse_header_line(&mut self, text: &str) -> Option<HunkCursor> {
        let file_ix = self.files.len() - 1;
        let file = &mut self.files[file_ix];
        if text.starts_with("new file mode") {
            file.change = Change::Added;
        } else if text.starts_with("deleted file mode") {
            file.change = Change::Deleted;
        } else if let Some(from) = text.strip_prefix("rename from ") {
            file.change = Change::Renamed {
                from: Self::unquote(from),
            };
        } else if let Some(from) = text.strip_prefix("copy from ") {
            file.change = Change::Copied {
                from: Self::unquote(from),
            };
        } else if let Some(to) = text
            .strip_prefix("rename to ")
            .or(text.strip_prefix("copy to "))
        {
            file.path = Self::unquote(to);
        } else if let Some(path) = text.strip_prefix("+++ ") {
            let path = Self::unquote(path.trim_end_matches('\t'));
            if let Some(path) = path.strip_prefix("b/") {
                file.path = path.to_string();
            }
        } else if let Some(blobs) = text.strip_prefix("index ") {
            file.old_blob = blobs
                .split_once("..")
                .map(|(old, _)| old.to_string())
                .filter(|old| old.bytes().any(|byte| byte != b'0'));
        } else if text.starts_with("Binary files ") || text == "GIT binary patch" {
            file.content = Content::Binary;
        } else if text.starts_with("@@ ") {
            let hunk = Hunk::parse(file_ix, text)?;
            let cursor = HunkCursor::new(&hunk);
            self.rows.push(Row::Hunk(hunk));
            return Some(cursor);
        }
        None
    }

    fn push_line(&mut self, cursor: &mut HunkCursor, text: &str) {
        let mut chars = text.chars();
        let kind = cursor.next_kind(chars.next());
        let file_ix = self.files.len() - 1;
        match kind {
            LineKind::Added { .. } => self.files[file_ix].added += 1,
            LineKind::Removed { .. } => self.files[file_ix].removed += 1,
            LineKind::Context { .. } => {}
        }
        self.rows.push(Row::Line {
            file: file_ix,
            line: DiffLine {
                kind,
                text: chars.as_str().to_string(),
                emphasis: Vec::new(),
            },
        });
    }

    /// Reads the path from `a/<path> b/<path>`; renames correct it from later header lines.
    fn path_from_git_header(paths: &str) -> String {
        if paths.starts_with('"') {
            let end = Self::closing_quote(paths).unwrap_or(paths.len() - 1);
            let path = Self::unquote(&paths[..=end]);
            return path.strip_prefix("a/").unwrap_or(&path).to_string();
        }
        let path_len = paths.len().saturating_sub(5) / 2;
        paths.get(2..2 + path_len).unwrap_or(paths).to_string()
    }

    fn closing_quote(quoted: &str) -> Option<usize> {
        let mut escaped = false;
        for (ix, ch) in quoted.char_indices().skip(1) {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => return Some(ix),
                _ => {}
            }
        }
        None
    }

    /// Decodes git's C-style quoting of unusual paths.
    fn unquote(path: &str) -> String {
        let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
            return path.to_string();
        };
        let mut bytes = Vec::with_capacity(inner.len());
        let mut input = inner.bytes();
        while let Some(byte) = input.next() {
            if byte != b'\\' {
                bytes.push(byte);
                continue;
            }
            match input.next() {
                Some(b't') => bytes.push(b'\t'),
                Some(b'n') => bytes.push(b'\n'),
                Some(b'r') => bytes.push(b'\r'),
                Some(first @ b'0'..=b'7') => {
                    let digit = |d: Option<u8>| d.map_or(0, |d| d.wrapping_sub(b'0'));
                    let (second, third) = (digit(input.next()), digit(input.next()));
                    bytes.push(((first - b'0') << 6) | (second << 3) | third);
                }
                Some(other) => bytes.push(other),
                None => {}
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(diff: &Diff) -> Vec<(LineKind, &str)> {
        diff.rows
            .iter()
            .filter_map(|row| match row {
                Row::Line { line, .. } => Some((line.kind, line.text.as_str())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn offsets_convert_across_expanded_tabs() {
        let line = DiffLine {
            kind: LineKind::Added { new: 1 },
            text: "\tå\tb".to_string(),
            emphasis: Vec::new(),
        };
        assert_eq!(line.display_text(), "    å    b");
        assert_eq!(line.display_offset(0), 0);
        assert_eq!(line.display_offset(1), 4);
        assert_eq!(line.display_offset(line.text.len()), 11);
        assert_eq!(line.source_offset(2), 0);
        assert_eq!(line.source_offset(4), 1);
        assert_eq!(line.source_offset(6), 3);
        assert_eq!(line.source_offset(11), line.text.len());
    }

    #[test]
    fn numbers_lines_on_both_sides() {
        let diff = Diff::parse(
            "diff --git a/src/a.rs b/src/a.rs\n\
             index 1..2 100644\n\
             --- a/src/a.rs\n\
             +++ b/src/a.rs\n\
             @@ -10,3 +10,3 @@ fn main() {\n\
             \x20keep\n\
             -old\n\
             +new\n\
             \x20tail\n",
        );
        assert_eq!(diff.files[0].path, "src/a.rs");
        assert_eq!(diff.files[0].change, Change::Modified);
        assert_eq!((diff.files[0].added, diff.files[0].removed), (1, 1));
        assert_eq!(
            lines(&diff),
            vec![
                (LineKind::Context { old: 10, new: 10 }, "keep"),
                (LineKind::Removed { old: 11 }, "old"),
                (LineKind::Added { new: 11 }, "new"),
                (LineKind::Context { old: 12, new: 12 }, "tail"),
            ]
        );
    }

    #[test]
    fn content_resembling_headers_stays_in_the_hunk() {
        let diff = Diff::parse(
            "diff --git a/a.md b/a.md\n\
             --- a/a.md\n\
             +++ b/a.md\n\
             @@ -1,2 +1,2 @@\n\
             --- a/gone\n\
             -diff --git a/x b/x\n\
             +++ b/here\n\
             +@@ -1 +1 @@\n",
        );
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "a.md");
        assert_eq!(
            lines(&diff),
            vec![
                (LineKind::Removed { old: 1 }, "-- a/gone"),
                (LineKind::Removed { old: 2 }, "diff --git a/x b/x"),
                (LineKind::Added { new: 1 }, "++ b/here"),
                (LineKind::Added { new: 2 }, "@@ -1 +1 @@"),
            ]
        );
    }

    #[test]
    fn ignores_no_newline_marker() {
        let diff = Diff::parse(
            "diff --git a/a b/a\n\
             --- a/a\n\
             +++ b/a\n\
             @@ -1 +1 @@\n\
             -old\n\
             \\ No newline at end of file\n\
             +new\n\
             \\ No newline at end of file\n\
             diff --git a/b b/b\n",
        );
        assert_eq!(lines(&diff).len(), 2);
        assert_eq!(diff.files.len(), 2);
    }

    #[test]
    fn reads_added_deleted_and_renamed_files() {
        let diff = Diff::parse(
            "diff --git a/new file b/new file\n\
             new file mode 100644\n\
             --- /dev/null\n\
             +++ b/new file\t\n\
             @@ -0,0 +1 @@\n\
             +x\n\
             diff --git a/dead b/dead\n\
             deleted file mode 100644\n\
             --- a/dead\n\
             +++ /dev/null\n\
             @@ -1 +0,0 @@\n\
             -x\n\
             diff --git a/before b/after\n\
             similarity index 100%\n\
             rename from before\n\
             rename to after\n",
        );
        assert_eq!(diff.files[0].path, "new file");
        assert_eq!(diff.files[0].change, Change::Added);
        assert_eq!(diff.files[1].path, "dead");
        assert_eq!(diff.files[1].change, Change::Deleted);
        assert_eq!(diff.files[2].path, "after");
        assert_eq!(
            diff.files[2].change,
            Change::Renamed {
                from: "before".to_string()
            }
        );
    }

    #[test]
    fn flags_content_without_hunks() {
        let diff = Diff::parse(
            "diff --git a/logo.png b/logo.png\n\
             Binary files a/logo.png and b/logo.png differ\n\
             diff --cc merged.rs\n\
             @@@ -1,1 -1,1 +1,1 @@@\n",
        );
        assert_eq!(diff.files[0].path, "logo.png");
        assert_eq!(diff.files[0].content, Content::Binary);
        assert_eq!(diff.files[1].path, "merged.rs");
        assert_eq!(diff.files[1].content, Content::Combined);
    }

    fn emphasis(diff: &Diff) -> Vec<Vec<&str>> {
        diff.rows
            .iter()
            .filter_map(|row| match row {
                Row::Line { line, .. } => Some(
                    line.emphasis
                        .iter()
                        .map(|range| &line.text[range.clone()])
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn emphasizes_words_changed_within_paired_lines() {
        let diff = Diff::parse(
            "diff --git a/a b/a\n\
             --- a/a\n\
             +++ b/a\n\
             @@ -1,3 +1,4 @@\n\
             -  components: { BaseMap },\n\
             -let total = old_value + 1;\n\
             -fn alpha() {}\n\
             +  components: { BaseMap, FavoriteMarkers },\n\
             +let total = new_value + 2;\n\
             +struct Unrelated;\n\
             +unpaired\n",
        );
        assert_eq!(
            emphasis(&diff),
            vec![
                vec![],
                vec!["old_value", "1"],
                vec![],
                vec![", FavoriteMarkers"],
                vec!["new_value", "2"],
                vec![],
                vec![],
            ]
        );
    }

    /// Two hunks of a 30-line file, hiding lines 1-9 and 13-24 (22-23 as new lines 21-22).
    const GAPPY: &str = "diff --git a/a b/a\n\
                         index 1234567..89abcde 100644\n\
                         --- a/a\n\
                         +++ b/a\n\
                         @@ -10,3 +10,2 @@ fn first()\n\
                         \x20line 10\n\
                         -line 11\n\
                         \x20line 12\n\
                         @@ -25,2 +24,2 @@ fn second()\n\
                         -line 25\n\
                         +changed 25\n\
                         \x20line 26\n";

    fn old_file() -> Vec<String> {
        (1..=30).map(|n| format!("line {n}")).collect()
    }

    fn headers(diff: &Diff) -> Vec<String> {
        (0..diff.rows.len())
            .filter(|row| matches!(diff.rows[*row], Row::Hunk(_)))
            .map(|row| diff.row_text(row).into_owned())
            .collect()
    }

    #[test]
    fn reads_the_old_blob_and_rebuilds_headers() {
        let diff = Diff::parse(GAPPY);
        assert_eq!(diff.files[0].old_blob.as_deref(), Some("1234567"));
        assert_eq!(
            headers(&diff),
            [
                "@@ -10,3 +10,2 @@ fn first()",
                "@@ -25,2 +24,2 @@ fn second()"
            ]
        );
        let added = Diff::parse("diff --git a/n b/n\nindex 0000000..1234567\n@@ -0,0 +1 @@\n+x\n");
        assert_eq!(added.files[0].old_blob, None);
        assert_eq!(headers(&added), ["@@ -0,0 +1 @@"]);
        assert!(added.gap_above(1).is_empty());
    }

    #[test]
    fn expanding_reveals_lines_above_then_merges_with_the_hunk_before() {
        let mut diff = Diff::parse(GAPPY);
        let second = 5;
        assert_eq!(diff.gap_above(second), 13..25);

        assert_eq!(diff.expand_above(second, &old_file(), 5), 6);
        assert_eq!(headers(&diff)[1], "@@ -20,7 +19,7 @@ fn second()");
        assert_eq!(
            lines(&diff)[3..5],
            [
                (LineKind::Context { old: 20, new: 19 }, "line 20"),
                (LineKind::Context { old: 21, new: 20 }, "line 21"),
            ]
        );

        assert_eq!(diff.expand_above(second, &old_file(), 20), 7);
        assert_eq!(headers(&diff), ["@@ -10,17 +10,16 @@ fn first()"]);
        assert_eq!(
            lines(&diff)[3],
            (LineKind::Context { old: 13, new: 12 }, "line 13")
        );
    }

    #[test]
    fn expanding_the_first_hunk_to_the_top_drops_its_header() {
        let mut diff = Diff::parse(GAPPY);
        assert_eq!(diff.gap_above(1), 1..10);
        assert_eq!(diff.expand_above(1, &old_file(), u32::MAX), 9);
        assert_eq!(headers(&diff), ["@@ -25,2 +24,2 @@ fn second()"]);
        assert_eq!(
            lines(&diff)[0],
            (LineKind::Context { old: 1, new: 1 }, "line 1")
        );
    }

    #[test]
    fn expanding_shifts_later_files() {
        let mut diff = Diff::parse(&format!("{GAPPY}diff --git a/b b/b\n"));
        let before = diff.files[1].first_row;
        diff.expand_above(1, &old_file(), 3);
        assert_eq!(diff.files[1].first_row, before + 3);
        assert_eq!(diff.rows[diff.files[1].first_row], Row::File(1));
    }

    #[test]
    fn decodes_quoted_paths() {
        let diff = Diff::parse("diff --git \"a/t\\tb\\303\\266.rs\" \"b/t\\tb\\303\\266.rs\"\n");
        assert_eq!(diff.files[0].path, "t\tbö.rs");
    }
}
