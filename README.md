# diffront

diffront is like `git diff` with a native GUI and allows you to do a local review with inline comments.

See a diff, leave inline comments, then copy them to your clipboard.

## Usage

```sh
cargo install --path .
diffront                  # all uncommitted work: `git diff HEAD` plus untracked files
diffront main...HEAD      # anything else is passed to `git diff`
diffront --cached
```

To run it as `git review foo..bar`, add a git alias:

```sh
git config --global alias.review '!f() { cd "${GIT_PREFIX:-.}" && diffront "$@"; }; f'
```

Click a line to comment on it. Press `Enter` to save, `Shift+Enter` for a new line, or `Esc` to
cancel. `⌘⇧C` copies the review as Markdown, and Finish review adds an overall comment. Tick
Reviewed on a file to mark and collapse it, or press `⌘⇧R` to do that for the file at the top of
the list. Click a file's header to just collapse it. The diff is
side by side; `⌘⇧D` or the toolbar button switches to inline. On Linux, use `Ctrl` in place of `⌘`.

Drag across text in a column to select it, and `⌘C` to copy. `⌘F` searches what's on screen,
including file paths and hunk headers but not the bodies of collapsed files. `Enter` and `Shift+Enter`
(or `⌘G` and `⌘⇧G`) move between matches, and `Esc` closes the search.

`j` and `k` scroll the diff.

The Files changed and Commits tabs (`⌘1`, `⌘2`) switch between the whole diff and one commit at a
time; `⌘[` and `⌘]` step through commits. Comments stay with the view you left them in, and all of
them are copied together.

Click the diff name in the toolbar to change the base, or press `⌘R` to refresh. Comments follow
their lines into the new diff. If a line is gone, diffront lists the affected comments and asks
before removing them. Reviewed marks are cleared from files that changed.
