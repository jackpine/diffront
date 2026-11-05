use std::ops::Range;

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
}

/// One entry in the flattened, scrollable diff.
#[derive(Debug, PartialEq)]
pub enum Row {
    File(usize),
    Hunk { file: usize, header: String },
    Line { file: usize, line: DiffLine },
}

impl Row {
    pub fn file(&self) -> usize {
        match self {
            Row::File(file) | Row::Hunk { file, .. } | Row::Line { file, .. } => *file,
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
    fn parse(header: &str) -> Option<Self> {
        let ranges = header.strip_prefix("@@ -")?;
        let (old, ranges) = ranges.split_once(" +")?;
        let (new, _) = ranges.split_once(" @@")?;
        let (old, old_remaining) = Self::parse_range(old)?;
        let (new, new_remaining) = Self::parse_range(new)?;
        Some(Self {
            old,
            new,
            old_remaining,
            new_remaining,
        })
    }

    fn parse_range(range: &str) -> Option<(u32, u32)> {
        match range.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((range.parse().ok()?, 1)),
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
        diff
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
        } else if text.starts_with("Binary files ") || text == "GIT binary patch" {
            file.content = Content::Binary;
        } else if text.starts_with("@@ ") {
            let cursor = HunkCursor::parse(text)?;
            self.rows.push(Row::Hunk {
                file: file_ix,
                header: text.to_string(),
            });
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

    #[test]
    fn decodes_quoted_paths() {
        let diff = Diff::parse("diff --git \"a/t\\tb\\303\\266.rs\" \"b/t\\tb\\303\\266.rs\"\n");
        assert_eq!(diff.files[0].path, "t\tbö.rs");
    }
}
