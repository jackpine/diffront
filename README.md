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

Click a line to comment on it. Press `⌘↵` to save or `Esc` to cancel, then `⌘⇧C` to copy every
comment as Markdown. On Linux, use `Ctrl` in place of `⌘`.
