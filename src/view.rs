use crate::diff::{Change, Content, Diff, DiffLine, LineKind, Row};
use crate::display::{DisplayList, Item, Layout};
use crate::git::{BaseOption, Commit, Repo};
use crate::review::{Carryover, Review};
use crate::symbols::{Symbol, Symbols};
use crate::text_area::TextArea;
use crate::theme::{MONO_FONT, Theme};
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, Hsla, KeyBinding,
    ListAlignment, ListOffset, ListState, SharedString, Stateful, Task, Window, actions, div, img,
    list, prelude::*, px, relative,
};
use std::collections::BTreeMap;
use std::time::Duration;

actions!(
    diffront,
    [
        CopyComments,
        SaveComment,
        CancelComment,
        Refresh,
        ToggleLayout,
        ShowFiles,
        ShowCommits,
        NextCommit,
        PreviousCommit
    ]
);

const COPIED_NOTICE: Duration = Duration::from_secs(2);
const STATUS_NOTICE: Duration = Duration::from_secs(5);
const LINE_NUMBER_WIDTH: f32 = 44.;
const INLINE_COMMENT_INDENT: f32 = 118.;
const SIDE_COMMENT_INDENT: f32 = 74.;
const FILE_GAP: f32 = 16.;
/// Rows kept above a revealed line, enough to clear the sticky file header.
const REVEAL_CONTEXT_ITEMS: usize = 3;

struct Draft {
    row: usize,
    input: Entity<TextArea>,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Files,
    Commits,
}

/// Identifies one of the diffs under review.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ViewKey {
    Files,
    Commit(String),
}

/// A review that isn't on screen, with where it was scrolled to.
struct Parked {
    review: Review,
    top: ListOffset,
}

struct BasePanel {
    input: Entity<TextArea>,
    options: Vec<BaseOption>,
    error: Option<SharedString>,
}

/// A loaded diff waiting for the user to accept losing some comments.
struct PendingUpdate {
    args: Vec<String>,
    diff: Diff,
    carryover: Carryover,
}

/// The main window: file list, scrollable diff, and comment editor.
pub struct DiffView {
    review: Review,
    active: ViewKey,
    parked: BTreeMap<ViewKey, Parked>,
    tab: Tab,
    commits: Vec<Commit>,
    selected_commit: Option<String>,
    commits_load: Option<Task<()>>,
    commit_load: Option<Task<()>>,
    overall: String,
    symbols: Symbols,
    repo: Repo,
    args: Vec<String>,
    layout: Layout,
    display: DisplayList,
    list: ListState,
    focus: FocusHandle,
    draft: Option<Draft>,
    finish_panel: Option<Entity<TextArea>>,
    base_panel: Option<BasePanel>,
    pending_update: Option<PendingUpdate>,
    loading: Option<Task<()>>,
    status: Option<(SharedString, Task<()>)>,
    copied_notice: Option<Task<()>>,
}

impl DiffView {
    pub fn new(
        review: Review,
        repo: Repo,
        args: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.set_global(Theme::for_appearance(window.appearance()));
        cx.observe_window_appearance(window, |_, window, cx| {
            cx.set_global(Theme::for_appearance(window.appearance()));
            cx.notify();
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus);
        let layout = Layout::SideBySide;
        let display = DisplayList::new(&review, layout);
        let list = ListState::new(display.len(), ListAlignment::Top, px(400.));
        let mut view = Self {
            review,
            active: ViewKey::Files,
            parked: BTreeMap::new(),
            tab: Tab::Files,
            commits: Vec::new(),
            selected_commit: None,
            commits_load: None,
            commit_load: None,
            overall: String::new(),
            symbols: Symbols::default(),
            repo,
            args,
            layout,
            display,
            list,
            focus,
            draft: None,
            finish_panel: None,
            base_panel: None,
            pending_update: None,
            loading: None,
            status: None,
            copied_notice: None,
        };
        view.reload_commits(window, cx);
        view
    }

    pub fn bind_keys(cx: &mut App) {
        cx.bind_keys([
            KeyBinding::new("secondary-shift-c", CopyComments, None),
            KeyBinding::new("secondary-r", Refresh, None),
            KeyBinding::new("secondary-1", ShowFiles, None),
            KeyBinding::new("secondary-2", ShowCommits, None),
            KeyBinding::new("secondary-]", NextCommit, None),
            KeyBinding::new("secondary-[", PreviousCommit, None),
            KeyBinding::new("secondary-shift-d", ToggleLayout, None),
            KeyBinding::new("enter", SaveComment, Some("TextArea")),
            KeyBinding::new("secondary-enter", SaveComment, Some("TextArea")),
            KeyBinding::new("escape", CancelComment, Some("DiffView")),
        ]);
    }

    fn open_draft(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft.as_ref().is_some_and(|draft| draft.row == row) {
            return;
        }
        self.save_draft(cx);
        let existing = self.review.comments.get(&row).cloned().unwrap_or_default();
        let input = cx.new(|cx| TextArea::new(existing, "Leave a comment…", cx));
        window.focus(&input.focus_handle(cx));
        self.draft = Some(Draft { row, input });

        // The next frame measures the row's new height, so reveal it in the one after.
        let list = self.list.clone();
        let list_ix = self.display.item_of(row);
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, _| {
                list.scroll_to_reveal_item(list_ix);
                window.refresh();
            });
        });
        cx.notify();
    }

    fn save_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.take() {
            self.review
                .set_comment(draft.row, draft.input.read(cx).text());
            cx.notify();
        }
    }

    fn close_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn save_comment(&mut self, _: &SaveComment, window: &mut Window, cx: &mut Context<Self>) {
        self.save_draft(cx);
        self.close_draft(window, cx);
    }

    fn cancel_comment(&mut self, _: &CancelComment, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_update.take().is_some() {
            cx.notify();
        } else if self.draft.is_some() {
            self.close_draft(window, cx);
        } else {
            self.close_finish_panel(window, cx);
        }
    }

    fn toggle_finish_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finish_panel.is_some() {
            self.close_finish_panel(window, cx);
            return;
        }
        self.save_draft(cx);
        self.base_panel = None;
        let overall = self.overall.clone();
        let input = cx.new(|cx| TextArea::new(overall, "Leave an overall comment…", cx));
        window.focus(&input.focus_handle(cx));
        self.finish_panel = Some(input);
        cx.notify();
    }

    fn save_overall(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = &self.finish_panel {
            self.overall = input.read(cx).text().trim().to_string();
        }
    }

    fn close_finish_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_overall(cx);
        self.finish_panel = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn copy_comments(&mut self, _: &CopyComments, window: &mut Window, cx: &mut Context<Self>) {
        self.save_draft(cx);
        self.close_draft(window, cx);
        self.close_finish_panel(window, cx);
        if !self.has_anything_to_copy() {
            return;
        }
        let reviews = self.reviews_in_order();
        let reviews: Vec<&Review> = reviews.into_iter().map(|(_, review)| review).collect();
        let export = Review::export_all(&self.overall, &reviews);
        cx.write_to_clipboard(ClipboardItem::new_string(export));
        self.copied_notice = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_NOTICE).await;
            this.update(cx, |this, cx| {
                this.copied_notice = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn review_for(&self, key: &ViewKey) -> Option<&Review> {
        if *key == self.active {
            Some(&self.review)
        } else {
            self.parked.get(key).map(|parked| &parked.review)
        }
    }

    /// Every loaded review in export order, starting with the whole diff.
    fn reviews_in_order(&self) -> Vec<(ViewKey, &Review)> {
        std::iter::once(ViewKey::Files)
            .chain(
                self.commits
                    .iter()
                    .map(|commit| ViewKey::Commit(commit.sha.clone())),
            )
            .filter_map(|key| {
                let review = self.review_for(&key)?;
                Some((key, review))
            })
            .collect()
    }

    fn comment_count(&self) -> usize {
        self.reviews_in_order()
            .iter()
            .map(|(_, review)| review.comments.len())
            .sum()
    }

    fn has_anything_to_copy(&self) -> bool {
        !self.overall.is_empty() || self.comment_count() > 0
    }

    fn showing_diff(&self) -> bool {
        match self.tab {
            Tab::Files => true,
            Tab::Commits => {
                let Some(sha) = &self.selected_commit else {
                    return false;
                };
                matches!(&self.active, ViewKey::Commit(active) if active == sha)
            }
        }
    }

    fn save_open_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft.is_some() {
            self.save_draft(cx);
            self.close_draft(window, cx);
        }
    }

    fn activate(&mut self, key: ViewKey, cx: &mut Context<Self>) {
        if key == self.active {
            return;
        }
        let Some(next) = self.parked.remove(&key) else {
            return;
        };
        let top = self.list.logical_scroll_top();
        let review = std::mem::replace(&mut self.review, next.review);
        let previous = std::mem::replace(&mut self.active, key);
        self.parked.insert(previous, Parked { review, top });
        self.display = DisplayList::new(&self.review, self.layout);
        self.list.reset(self.display.len());
        self.list.scroll_to(next.top);
        cx.notify();
    }

    fn show_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab || self.pending_update.is_some() {
            return;
        }
        self.save_open_draft(window, cx);
        self.tab = tab;
        match tab {
            Tab::Files => {
                self.commit_load = None;
                self.activate(ViewKey::Files, cx);
            }
            Tab::Commits => self.select_listed_commit(window, cx),
        }
        cx.notify();
    }

    /// Shows the selected commit, or the first one if it's no longer listed.
    fn select_listed_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self
            .selected_commit
            .as_ref()
            .filter(|sha| self.commits.iter().any(|commit| commit.sha == **sha))
            .or(self.commits.first().map(|commit| &commit.sha))
            .cloned();
        match selected {
            Some(sha) => self.show_commit(sha, window, cx),
            None => self.selected_commit = None,
        }
    }

    fn show_commit(&mut self, sha: String, window: &mut Window, cx: &mut Context<Self>) {
        self.save_open_draft(window, cx);
        self.selected_commit = Some(sha.clone());
        let key = ViewKey::Commit(sha.clone());
        if self.review_for(&key).is_some() {
            self.commit_load = None;
            self.activate(key, cx);
            return;
        }
        let repo = self.repo.clone();
        let patch = {
            let sha = sha.clone();
            cx.background_spawn(async move { repo.commit_patch(&sha) })
        };
        self.commit_load = Some(cx.spawn_in(window, async move |this, cx| {
            let patch = patch.await;
            this.update_in(cx, |this, _, cx| {
                this.commit_load = None;
                this.finish_commit_load(sha, patch, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    fn finish_commit_load(
        &mut self,
        sha: String,
        patch: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let patch = match patch {
            Ok(patch) => patch,
            Err(error) => {
                self.show_status(format!("Couldn't load commit: {error}"), cx);
                return;
            }
        };
        let source = self
            .commits
            .iter()
            .find(|commit| commit.sha == sha)
            .map_or_else(
                || format!("commit {sha}"),
                |commit| format!("commit {} ({})", commit.short, commit.subject),
            );
        let key = ViewKey::Commit(sha);
        self.parked.insert(
            key.clone(),
            Parked {
                review: Review::new(source, Diff::parse(&patch)),
                top: ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.),
                },
            },
        );
        self.activate(key, cx);
    }

    fn step_commit(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab != Tab::Commits {
            return;
        }
        let current = self
            .selected_commit
            .as_ref()
            .and_then(|sha| self.commits.iter().position(|commit| commit.sha == *sha));
        let target = current
            .and_then(|current| current.checked_add_signed(delta))
            .and_then(|target| self.commits.get(target))
            .map(|commit| commit.sha.clone());
        if let Some(sha) = target {
            self.show_commit(sha, window, cx);
        }
    }

    fn reload_commits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        let args = self.args.clone();
        let commits = cx.background_spawn(async move { repo.commits(&args).unwrap_or_default() });
        self.commits_load = Some(cx.spawn_in(window, async move |this, cx| {
            let commits = commits.await;
            this.update_in(cx, |this, window, cx| {
                this.commits_load = None;
                this.finish_commits(commits, window, cx);
            })
            .ok();
        }));
    }

    /// Replaces the commit list, keeping commits with comments even if they left the range.
    fn finish_commits(
        &mut self,
        mut commits: Vec<Commit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for commit in std::mem::take(&mut self.commits) {
            let listed = commits.iter().any(|new| new.sha == commit.sha);
            let has_comments = self
                .review_for(&ViewKey::Commit(commit.sha.clone()))
                .is_some_and(|review| !review.comments.is_empty());
            if !listed && has_comments {
                commits.push(Commit {
                    in_range: false,
                    ..commit
                });
            }
        }
        self.commits = commits;
        if self.tab == Tab::Commits {
            self.select_listed_commit(window, cx);
        }
        cx.notify();
    }

    fn reveal_comment(
        &mut self,
        key: ViewKey,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_open_draft(window, cx);
        self.commit_load = None;
        match &key {
            ViewKey::Files => self.tab = Tab::Files,
            ViewKey::Commit(sha) => {
                self.tab = Tab::Commits;
                self.selected_commit = Some(sha.clone());
            }
        }
        self.activate(key, cx);
        self.reveal_row(row, window, cx);
    }

    fn jump_to_file(&mut self, file: usize, cx: &mut Context<Self>) {
        self.jump_to_row(self.review.diff.files[file].first_row, cx);
    }

    fn jump_to_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let file = self.review.diff.rows[row].file();
        let row = if self.review.collapsed.contains(&file) {
            self.review.diff.files[file].first_row
        } else {
            row
        };
        self.list.scroll_to(ListOffset {
            item_ix: self.display.item_of(row),
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    fn reveal_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.set_collapsed(self.review.diff.rows[row].file(), false, window, cx);
        self.list.scroll_to(ListOffset {
            item_ix: self
                .display
                .item_of(row)
                .saturating_sub(REVEAL_CONTEXT_ITEMS),
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    fn toggle_layout(&mut self, _: &ToggleLayout, _: &mut Window, cx: &mut Context<Self>) {
        let top_row = self.top_row();
        self.layout = self.layout.toggled();
        self.display = DisplayList::new(&self.review, self.layout);
        self.list.reset(self.display.len());
        if let Some(row) = top_row {
            self.jump_to_row(row, cx);
        }
        cx.notify();
    }

    /// The file whose real header has scrolled away, and how far the next header pushes it up.
    fn sticky_header(&self) -> Option<(usize, gpui::Pixels)> {
        let top = self.list.logical_scroll_top();
        let row = self.display.first_row_at(top.item_ix)?;
        let row = &self.review.diff.rows[row];
        if matches!(row, Row::File(_)) && top.offset_in_item < px(FILE_GAP) {
            return None;
        }
        let file = row.file();
        let viewport_top = self.list.viewport_bounds().top();
        let push = self
            .review
            .diff
            .files
            .get(file + 1)
            .and_then(|next| {
                self.list
                    .bounds_for_item(self.display.item_of(next.first_row))
            })
            .map_or(px(0.), |next| {
                let header_height = next.size.height - px(FILE_GAP);
                (next.top() - viewport_top - header_height).min(px(0.))
            });
        Some((file, push))
    }

    fn top_row(&self) -> Option<usize> {
        self.display
            .first_row_at(self.list.logical_scroll_top().item_ix)
    }

    fn toggle_reviewed(&mut self, file: usize, window: &mut Window, cx: &mut Context<Self>) {
        let reviewed = self.review.reviewed.insert(file);
        if !reviewed {
            self.review.reviewed.remove(&file);
        }
        self.set_collapsed(file, reviewed, window, cx);
        cx.notify();
    }

    fn toggle_collapsed(&mut self, file: usize, window: &mut Window, cx: &mut Context<Self>) {
        let collapsed = !self.review.collapsed.contains(&file);
        self.set_collapsed(file, collapsed, window, cx);
    }

    fn set_collapsed(
        &mut self,
        file: usize,
        collapsed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.review.collapsed.contains(&file) == collapsed {
            return;
        }
        let body = self.review.diff.file_body(file);
        if collapsed
            && self
                .draft
                .as_ref()
                .is_some_and(|draft| body.contains(&draft.row))
        {
            self.save_draft(cx);
            self.close_draft(window, cx);
        }
        let header = self.display.item_of(self.review.diff.files[file].first_row);
        let first_body_item = header + 1;
        let old_len = self.display.len();
        if collapsed {
            self.review.collapsed.insert(file);
        } else {
            self.review.collapsed.remove(&file);
        }
        self.display = DisplayList::new(&self.review, self.layout);
        if collapsed {
            let hidden = first_body_item..first_body_item + (old_len - self.display.len());
            let scrolled_into_body = hidden.contains(&self.list.logical_scroll_top().item_ix);
            self.list.splice(hidden, 0);
            if scrolled_into_body {
                self.list.scroll_to(ListOffset {
                    item_ix: header,
                    offset_in_item: px(0.),
                });
            }
        } else {
            self.list.splice(
                first_body_item..first_body_item,
                self.display.len() - old_len,
            );
        }
        cx.notify();
    }

    fn refresh(&mut self, _: &Refresh, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_commits(window, cx);
        if self.tab == Tab::Files {
            self.load(self.args.clone(), window, cx);
        }
        cx.notify();
    }

    fn toggle_base_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.base_panel.take().is_some() {
            window.focus(&self.focus);
        } else {
            self.save_draft(cx);
            self.close_finish_panel(window, cx);
            let args = self.args.join(" ");
            let input = cx.new(|cx| TextArea::new(args, "Blank for all uncommitted work", cx));
            window.focus(&input.focus_handle(cx));
            self.base_panel = Some(BasePanel {
                input,
                options: self.repo.base_options(),
                error: None,
            });
        }
        cx.notify();
    }

    fn apply_base(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = &self.base_panel {
            let text = panel.input.read(cx).text();
            let args = text.split_whitespace().map(str::to_string).collect();
            self.load(args, window, cx);
        }
    }

    fn load(&mut self, args: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft.is_some() {
            self.save_draft(cx);
            self.close_draft(window, cx);
            if let Some(panel) = &self.base_panel {
                window.focus(&panel.input.focus_handle(cx));
            }
        }
        let repo = self.repo.clone();
        let patch = {
            let args = args.clone();
            cx.background_spawn(async move { repo.patch(&args) })
        };
        self.loading = Some(cx.spawn_in(window, async move |this, cx| {
            let patch = patch.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = None;
                this.finish_load(args, patch, window, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    fn finish_load(
        &mut self,
        args: Vec<String>,
        patch: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let patch = match patch {
            Ok(patch) => patch,
            Err(error) => {
                if let Some(panel) = &mut self.base_panel {
                    panel.error = Some(error.into());
                } else {
                    self.show_status(format!("Refresh failed: {error}"), cx);
                }
                cx.notify();
                return;
            }
        };
        self.show_tab(Tab::Files, window, cx);
        let diff = Diff::parse(&patch);
        if args == self.args && diff == self.review.diff {
            self.close_base_panel(window, cx);
            self.show_status("Already up to date".to_string(), cx);
            return;
        }
        let carryover = self.review.carry_over(&diff);
        let update = PendingUpdate {
            args,
            diff,
            carryover,
        };
        self.close_base_panel(window, cx);
        if update.carryover.lost.is_empty() {
            self.apply_update(update, window, cx);
        } else {
            self.pending_update = Some(update);
            cx.notify();
        }
    }

    fn close_base_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.base_panel.take().is_some() {
            window.focus(&self.focus);
            cx.notify();
        }
    }

    fn apply_update(&mut self, update: PendingUpdate, window: &mut Window, cx: &mut Context<Self>) {
        let PendingUpdate {
            args,
            diff,
            carryover,
        } = update;
        let unmarked_files = carryover.unmarked_files;
        let top_row = self.top_row();
        let new_top =
            top_row.and_then(|row| self.review.matching_rows(row, &diff).first().copied());

        self.review
            .apply_update(Repo::describe(&args), diff, carryover);
        let base_changed = args != self.args;
        self.args = args;
        if base_changed {
            self.reload_commits(window, cx);
        }
        self.display = DisplayList::new(&self.review, self.layout);
        self.list.reset(self.display.len());
        if let Some(row) = new_top {
            self.jump_to_row(row, cx);
        }
        let status = match unmarked_files {
            0 => "Diff updated".to_string(),
            1 => "Diff updated. 1 file changed, so it's no longer marked reviewed".to_string(),
            count => {
                format!("Diff updated. {count} files changed, so they're no longer marked reviewed")
            }
        };
        self.show_status(status, cx);
    }

    fn show_status(&mut self, text: String, cx: &mut Context<Self>) {
        let clear = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STATUS_NOTICE).await;
            this.update(cx, |this, cx| {
                this.status = None;
                cx.notify();
            })
            .ok();
        });
        self.status = Some((text.into(), clear));
        cx.notify();
    }

    fn accept_pending_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(update) = self.pending_update.take() {
            self.apply_update(update, window, cx);
        }
    }

    fn button(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        theme: &Theme,
    ) -> Stateful<gpui::Div> {
        Self::button_frame(id, theme).child(label.into())
    }

    fn symbol_button(
        &self,
        id: impl Into<SharedString>,
        symbol: Symbol,
        label: impl Into<SharedString>,
        theme: &Theme,
    ) -> Stateful<gpui::Div> {
        Self::button_frame(id, theme)
            .children(self.icon(symbol, theme.text))
            .child(label.into())
    }

    fn icon(&self, symbol: Symbol, color: Hsla) -> Option<impl IntoElement + use<>> {
        let image = self.symbols.image(symbol, color)?;
        Some(img(image).size(px(14.)).flex_shrink_0())
    }

    fn button_frame(id: impl Into<SharedString>, theme: &Theme) -> Stateful<gpui::Div> {
        div()
            .id(id.into())
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .text_xs()
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
    }

    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let files = &self.review.diff.files;
        let added: usize = files.iter().map(|file| file.added).sum();
        let removed: usize = files.iter().map(|file| file.removed).sum();
        let count = self.comment_count();
        let shortcut = if cfg!(target_os = "macos") {
            "⌘⇧C"
        } else {
            "Ctrl+Shift+C"
        };
        let copy_label = if self.copied_notice.is_some() {
            "Copied to clipboard".to_string()
        } else if !self.overall.is_empty() {
            format!("Copy review  {shortcut}")
        } else {
            format!(
                "Copy {count} comment{}  {shortcut}",
                if count == 1 { "" } else { "s" }
            )
        };
        let can_copy = self.has_anything_to_copy();
        let status = self.status.as_ref().map(|(status, _)| status.clone());
        let tabs = div()
            .flex()
            .flex_shrink_0()
            .rounded_sm()
            .border_1()
            .border_color(theme.border)
            .child(self.render_tab(Tab::Files, None, theme, cx))
            .child(self.render_tab(Tab::Commits, Some(self.commits.len()), theme, cx));
        let (layout_symbol, layout_label) = match self.layout {
            Layout::SideBySide => (Symbol::Inline, "Inline view"),
            Layout::Inline => (Symbol::SideBySide, "Side-by-side view"),
        };
        let refresh_label = if self.loading.is_some() {
            "Loading…"
        } else {
            "Refresh"
        };
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .h(px(40.))
            .px_3()
            .bg(theme.panel)
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            .child(
                Self::button(
                    "change-base",
                    format!("{}  ▾", Repo::describe(&self.args)),
                    theme,
                )
                .debug_selector(|| "change-base".to_string())
                .font_family(MONO_FONT)
                .py_1()
                .on_click(cx.listener(|this, _, window, cx| this.toggle_base_panel(window, cx))),
            )
            .child(tabs)
            .child(div().text_xs().text_color(theme.muted).child(format!(
                "{} file{}",
                files.len(),
                if files.len() == 1 { "" } else { "s" }
            )))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.added)
                    .child(format!("+{added}")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.removed)
                    .child(format!("−{removed}")),
            )
            .child(div().text_xs().text_color(theme.muted).child(format!(
                "{}/{} reviewed",
                self.review.reviewed.len(),
                files.len()
            )))
            .child(div().flex_1())
            .children(status.map(|status| div().text_xs().text_color(theme.muted).child(status)))
            .child(
                self.symbol_button("toggle-layout", layout_symbol, layout_label, theme)
                    .debug_selector(|| "toggle-layout".to_string())
                    .py_1()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_layout(&ToggleLayout, window, cx)
                    })),
            )
            .child(
                self.symbol_button("refresh", Symbol::Refresh, refresh_label, theme)
                    .debug_selector(|| "refresh".to_string())
                    .py_1()
                    .on_click(
                        cx.listener(|this, _, window, cx| this.refresh(&Refresh, window, cx)),
                    ),
            )
            .child(
                self.symbol_button("finish-review", Symbol::Finish, "Finish review", theme)
                    .debug_selector(|| "finish-review".to_string())
                    .py_1()
                    .on_click(
                        cx.listener(|this, _, window, cx| this.toggle_finish_panel(window, cx)),
                    ),
            )
            .child(
                div()
                    .id("copy-comments")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .when(!can_copy, |button| {
                        button.bg(theme.hover).text_color(theme.muted)
                    })
                    .when(can_copy, |button| {
                        button
                            .bg(theme.accent)
                            .text_color(theme.on_accent)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.copy_comments(&CopyComments, window, cx)
                            }))
                    })
                    .children(self.icon(
                        Symbol::Copy,
                        if can_copy {
                            theme.on_accent
                        } else {
                            theme.muted
                        },
                    ))
                    .child(copy_label),
            )
    }

    fn render_tab(
        &self,
        tab: Tab,
        count: Option<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (id, symbol, label) = match tab {
            Tab::Files => ("tab-files", Symbol::Files, "Files changed"),
            Tab::Commits => ("tab-commits", Symbol::Commits, "Commits"),
        };
        let hover = theme.hover;
        div()
            .id(id)
            .debug_selector(|| id.to_string())
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .text_xs()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .when(self.tab == tab, |item| item.bg(theme.selected))
            .on_click(cx.listener(move |this, _, window, cx| this.show_tab(tab, window, cx)))
            .children(self.icon(symbol, theme.text))
            .child(label)
            .children(count.map(|count| div().text_color(theme.muted).child(count.to_string())))
    }

    fn render_commit_list(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = theme.hover;
        let items: Vec<_> = self
            .commits
            .iter()
            .map(|commit| {
                let sha = commit.sha.clone();
                let comments = self
                    .review_for(&ViewKey::Commit(sha.clone()))
                    .map_or(0, |review| review.comments.len());
                let selected = self.selected_commit.as_ref() == Some(&sha);
                let detail = if commit.in_range {
                    commit.short.clone()
                } else {
                    format!("{} · not in range", commit.short)
                };
                div()
                    .id(SharedString::from(format!("commit-{sha}")))
                    .debug_selector(|| format!("commit-{}", commit.subject))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .when(selected, |item| item.bg(theme.selected))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_commit(sha.clone(), window, cx)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(div().truncate().text_sm().child(commit.subject.clone()))
                            .child(
                                div()
                                    .truncate()
                                    .font_family(MONO_FONT)
                                    .text_xs()
                                    .text_color(theme.muted)
                                    .child(detail),
                            ),
                    )
                    .when(comments > 0, |item| {
                        item.child(
                            div()
                                .flex_shrink_0()
                                .px_1p5()
                                .rounded_full()
                                .bg(theme.accent)
                                .text_color(theme.on_accent)
                                .text_xs()
                                .child(comments.to_string()),
                        )
                    })
            })
            .collect();
        div()
            .id("commits")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .max_h(relative(0.45))
            .py_1()
            .overflow_y_scroll()
            .border_b_1()
            .border_color(theme.border)
            .children(items)
    }

    fn render_commit_bar(
        &self,
        commit: &Commit,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let position = self
            .commits
            .iter()
            .position(|candidate| candidate.sha == commit.sha)
            .map_or(0, |ix| ix + 1);
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .px_3()
            .py_1p5()
            .bg(theme.panel)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(MONO_FONT)
                    .text_xs()
                    .text_color(theme.muted)
                    .child(commit.short.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_sm()
                    .child(commit.subject.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(theme.muted)
                    .child(format!(
                        "{} · {} · {position} of {}",
                        commit.author,
                        commit.when,
                        self.commits.len()
                    )),
            )
            .child(
                self.symbol_button("previous-commit", Symbol::Previous, "Previous", theme)
                    .debug_selector(|| "previous-commit".to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.step_commit(-1, window, cx))),
            )
            .child(
                self.symbol_button("next-commit", Symbol::Next, "Next", theme)
                    .debug_selector(|| "next-commit".to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.step_commit(1, window, cx))),
            )
    }

    fn render_sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.top_row().map(|row| self.review.diff.rows[row].file());
        let items = self.review.diff.files.iter().enumerate().map(|(ix, file)| {
            let (dir, name) = file
                .path
                .rsplit_once('/')
                .map_or(("", file.path.as_str()), |(dir, name)| (dir, name));
            let comments = self.review.comments_in_file(ix);
            div()
                .id(("file", ix))
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .cursor_pointer()
                .when(active == Some(ix), |item| item.bg(theme.selected))
                .hover({
                    let hover = theme.hover;
                    move |style| style.bg(hover)
                })
                .on_click(cx.listener(move |this, _, _, cx| this.jump_to_file(ix, cx)))
                .when(self.review.reviewed.contains(&ix), |item| {
                    item.text_color(theme.muted)
                        .child(div().flex_shrink_0().text_color(theme.added).child("✓"))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .child(div().truncate().text_sm().child(name.to_string()))
                        .when(!dir.is_empty(), |item| {
                            item.child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted)
                                    .child(dir.to_string()),
                            )
                        }),
                )
                .when(comments > 0, |item| {
                    item.child(
                        div()
                            .flex_shrink_0()
                            .px_1p5()
                            .rounded_full()
                            .bg(theme.accent)
                            .text_color(theme.on_accent)
                            .text_xs()
                            .child(comments.to_string()),
                    )
                })
                .child(Self::render_stats(file.added, file.removed, theme))
        });
        let items: Vec<_> = items.collect();
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(280.))
            .h_full()
            .bg(theme.panel)
            .border_r_1()
            .border_color(theme.border)
            .when(self.tab == Tab::Commits, |sidebar| {
                sidebar.child(self.render_commit_list(theme, cx))
            })
            .when(self.showing_diff(), |sidebar| {
                sidebar.child(
                    div()
                        .id("files")
                        .flex_1()
                        .min_h(px(0.))
                        .py_1()
                        .overflow_y_scroll()
                        .children(items),
                )
            })
    }

    fn render_main(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let commit = self
            .selected_commit
            .as_ref()
            .filter(|_| self.tab == Tab::Commits)
            .and_then(|sha| self.commits.iter().find(|commit| commit.sha == *sha));
        let placeholder = if !self.showing_diff() {
            Some(
                if self.commit_load.is_some() || self.commits_load.is_some() {
                    "Loading…"
                } else {
                    "No commits in this range"
                },
            )
        } else if self.display.len() == 0 {
            Some("No changes")
        } else {
            None
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .children(commit.map(|commit| self.render_commit_bar(commit, theme, cx)))
            .map(|main| match placeholder {
                Some(text) => main.child(
                    div()
                        .flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(theme.muted)
                        .child(text),
                ),
                None => main.child(
                    div()
                        .relative()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.))
                        .w_full()
                        .overflow_hidden()
                        .child(
                            list(
                                self.list.clone(),
                                cx.processor(|this, ix, window, cx| {
                                    this.render_row(ix, window, cx)
                                }),
                            )
                            .flex_1()
                            .min_h(px(0.))
                            .w_full(),
                        )
                        .children(self.sticky_header().map(|(file, push)| {
                            div()
                                .id("sticky-header")
                                .debug_selector(|| "sticky-header".to_string())
                                .absolute()
                                .top(push)
                                .left_0()
                                .right_0()
                                .block_mouse_except_scroll()
                                .child(self.render_file_header(file, theme, cx))
                        })),
                ),
            })
    }

    fn render_stats(added: usize, removed: usize, theme: &Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_shrink_0()
            .gap_1()
            .text_xs()
            .when(added > 0, |stats| {
                stats.child(div().text_color(theme.added).child(format!("+{added}")))
            })
            .when(removed > 0, |stats| {
                stats.child(div().text_color(theme.removed).child(format!("−{removed}")))
            })
    }

    fn render_row(&mut self, list_ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let row = div().w_full();
        let item = self.display.item(list_ix);
        let Item::Full(ix) = item else {
            return row
                .child(self.render_side_by_side(item, &theme, cx))
                .into_any_element();
        };
        match &self.review.diff.rows[ix] {
            Row::File(file) => row
                .pt(px(FILE_GAP))
                .child(self.render_file_header(*file, &theme, cx))
                .into_any_element(),
            Row::Hunk { header, .. } => row
                .px_3()
                .py_0p5()
                .bg(theme.hunk_bg)
                .text_color(theme.muted)
                .font_family(MONO_FONT)
                .text_xs()
                .child(header.clone())
                .into_any_element(),
            Row::Line { line, .. } => row
                .child(self.render_inline_line(ix, line, &theme, cx))
                .into_any_element(),
        }
    }

    fn render_chevron(&self, collapsed: bool, theme: &Theme) -> impl IntoElement {
        let (symbol, glyph) = if collapsed {
            (Symbol::Collapsed, "▸")
        } else {
            (Symbol::Expanded, "▾")
        };
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(px(18.))
            .text_lg()
            .text_color(theme.muted)
            .map(|chevron| match self.symbols.image(symbol, theme.muted) {
                Some(image) => chevron.child(img(image).size_full()),
                None => chevron.child(glyph),
            })
    }

    fn render_file_header(
        &self,
        file_ix: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let file = &self.review.diff.files[file_ix];
        let reviewed = self.review.reviewed.contains(&file_ix);
        let collapsed = self.review.collapsed.contains(&file_ix);
        let change = match &file.change {
            Change::Modified => None,
            Change::Added => Some("new file".to_string()),
            Change::Deleted => Some("deleted".to_string()),
            Change::Renamed { from } => Some(format!("renamed from {from}")),
            Change::Copied { from } => Some(format!("copied from {from}")),
        };
        let notice = match file.content {
            Content::Text => None,
            Content::Binary => Some("Binary file, not shown"),
            Content::Combined => Some("Combined merge diff, not shown"),
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_1p5()
            .bg(theme.panel)
            .border_t_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id(("file-header", file_ix))
                    .debug_selector(|| format!("file-header-{file_ix}"))
                    .flex()
                    .flex_1()
                    .min_w(px(0.))
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_collapsed(file_ix, window, cx)
                    }))
                    .child(self.render_chevron(collapsed, theme))
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .font_family(MONO_FONT)
                            .text_sm()
                            .child(file.path.clone()),
                    )
                    .children(change.into_iter().map(|change| {
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted)
                            .child(change)
                    }))
                    .children(notice.into_iter().map(|notice| {
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted)
                            .child(notice)
                    }))
                    .child(div().flex_1())
                    .child(div().flex_shrink_0().child(Self::render_stats(
                        file.added,
                        file.removed,
                        theme,
                    ))),
            )
            .child(
                div()
                    .id(("reviewed", file_ix))
                    .flex_shrink_0()
                    .debug_selector(|| format!("reviewed-{file_ix}"))
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_reviewed(file_ix, window, cx)
                    }))
                    .child(
                        div()
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .border_1()
                            .when(reviewed, |check| {
                                check
                                    .bg(theme.accent)
                                    .border_color(theme.accent)
                                    .text_color(theme.on_accent)
                                    .child("✓")
                            })
                            .when(!reviewed, |check| check.border_color(theme.muted)),
                    )
                    .child("Reviewed"),
            )
    }

    fn render_inline_line(
        &self,
        ix: usize,
        line: &DiffLine,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let numbers = match line.kind {
            LineKind::Context { old, new } => [Some(old), Some(new)],
            LineKind::Added { new } => [None, Some(new)],
            LineKind::Removed { old } => [Some(old), None],
        };
        div()
            .flex()
            .flex_col()
            .child(self.render_line_cell(ix, line, "line", &numbers, theme, cx))
            .children(self.render_annotation(ix, theme, cx))
    }

    fn render_side_by_side(
        &self,
        item: Item,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (left, right) = item.sides();
        // A context line appears in both columns, but its comment only belongs under one.
        let left_note = left
            .filter(|row| Some(*row) != right)
            .and_then(|row| self.render_annotation(row, theme, cx));
        let right_note = right.and_then(|row| self.render_annotation(row, theme, cx));
        let has_notes = left_note.is_some() || right_note.is_some();
        let column = || div().flex_1().min_w(px(0.));
        let divider = || div().flex_shrink_0().w(px(1.)).bg(theme.border);
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .child(self.render_side_cell(left, "left", theme, cx))
                    .child(divider())
                    .child(self.render_side_cell(right, "right", theme, cx)),
            )
            .when(has_notes, |item| {
                item.child(
                    div()
                        .flex()
                        .child(column().children(left_note))
                        .child(divider())
                        .child(column().children(right_note)),
                )
            })
    }

    fn render_side_cell(
        &self,
        row: Option<usize>,
        side: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = row else {
            return div()
                .flex_1()
                .min_w(px(0.))
                .bg(theme.panel)
                .into_any_element();
        };
        let Row::Line { line, .. } = &self.review.diff.rows[row] else {
            unreachable!("side-by-side items only refer to line rows");
        };
        let number = match (side, line.kind) {
            ("left", LineKind::Context { old, .. }) | (_, LineKind::Removed { old }) => old,
            (_, LineKind::Context { new, .. } | LineKind::Added { new }) => new,
        };
        self.render_line_cell(row, line, side, &[Some(number)], theme, cx)
            .flex_1()
            .min_w(px(0.))
            .into_any_element()
    }

    fn render_line_cell(
        &self,
        ix: usize,
        line: &DiffLine,
        id: &'static str,
        numbers: &[Option<u32>],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let (marker, background, marker_color): (_, Option<Hsla>, _) = match line.kind {
            LineKind::Context { .. } => (" ", None, theme.muted),
            LineKind::Added { .. } => ("+", Some(theme.added_bg), theme.added),
            LineKind::Removed { .. } => ("-", Some(theme.removed_bg), theme.removed),
        };
        let group: SharedString = format!("{id}-{ix}").into();
        let accent = theme.accent;
        let hover = theme.hover;
        div()
            .id((id, ix))
            .group(group.clone())
            .flex()
            .font_family(MONO_FONT)
            .text_xs()
            .line_height(px(20.))
            .cursor_pointer()
            .when_some(background, |row, background| row.bg(background))
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _, window, cx| this.open_draft(ix, window, cx)))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(18.))
                    .flex()
                    .justify_center()
                    .text_color(gpui::transparent_black())
                    .group_hover(group, move |style| style.text_color(accent))
                    .child("+"),
            )
            .children(numbers.iter().map(|number| {
                div()
                    .flex_shrink_0()
                    .w(px(LINE_NUMBER_WIDTH))
                    .pr_2()
                    .flex()
                    .justify_end()
                    .text_color(theme.muted)
                    .child(number.map_or(String::new(), |number| number.to_string()))
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(12.))
                    .text_color(marker_color)
                    .child(marker),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .pr_3()
                    .child(line.text.replace('\t', "    ")),
            )
    }

    fn render_annotation(
        &self,
        ix: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(draft) = self.draft.as_ref().filter(|draft| draft.row == ix) {
            return Some(self.render_editor(draft, theme, cx).into_any_element());
        }
        let comment = self.review.comments.get(&ix)?;
        Some(
            self.annotation_card(theme)
                .child(div().text_sm().child(comment.clone()))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .mt_2()
                        .child(Self::button(format!("edit-{ix}"), "Edit", theme).on_click(
                            cx.listener(move |this, _, window, cx| this.open_draft(ix, window, cx)),
                        ))
                        .child(
                            Self::button(format!("delete-{ix}"), "Delete", theme).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.review.comments.remove(&ix);
                                    cx.notify();
                                }),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_editor(
        &self,
        draft: &Draft,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hint = if cfg!(target_os = "macos") {
            "↵ to save · ⇧↵ for a new line · Esc to cancel"
        } else {
            "Enter to save · Shift+Enter for a new line · Esc to cancel"
        };
        self.annotation_card(theme)
            .child(
                div()
                    .debug_selector(|| "comment-input".to_string())
                    .min_h(px(60.))
                    .p_2()
                    .rounded_sm()
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.accent)
                    .text_sm()
                    .line_height(px(20.))
                    .child(draft.input.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mt_2()
                    .child(
                        Self::button("save-comment", "Save", theme).on_click(cx.listener(
                            |this, _, window, cx| this.save_comment(&SaveComment, window, cx),
                        )),
                    )
                    .child(
                        Self::button("cancel-comment", "Cancel", theme).on_click(cx.listener(
                            |this, _, window, cx| this.cancel_comment(&CancelComment, window, cx),
                        )),
                    )
                    .child(div().text_xs().text_color(theme.muted).child(hint)),
            )
    }

    fn render_finish_panel(
        &self,
        input: &Entity<TextArea>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hint = if cfg!(target_os = "macos") {
            "↵ to copy · ⇧↵ for a new line · Esc to close"
        } else {
            "Enter to copy · Shift+Enter for a new line · Esc to close"
        };
        let hover = theme.hover;
        let mut entries = Vec::new();
        for (key, review) in self.reviews_in_order() {
            let prefix = match &key {
                ViewKey::Files => String::new(),
                ViewKey::Commit(sha) => self
                    .commits
                    .iter()
                    .find(|commit| commit.sha == *sha)
                    .map_or(String::new(), |commit| format!("{} ", commit.short)),
            };
            for entry in review.entries() {
                let row = entry.row;
                let key = key.clone();
                entries.push(
                    div()
                        .id(("summary", entries.len()))
                        .debug_selector(|| format!("summary-{prefix}{row}"))
                        .flex_shrink_0()
                        .px_3()
                        .py_1p5()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.reveal_comment(key.clone(), row, window, cx)
                        }))
                        .child(
                            div()
                                .truncate()
                                .font_family(MONO_FONT)
                                .text_xs()
                                .text_color(theme.muted)
                                .child(format!("{prefix}{}", entry.location)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_ellipsis()
                                .line_clamp(2)
                                .child(entry.comment.replace('\n', " ")),
                        ),
                );
            }
        }
        let count = self.comment_count();
        div()
            .absolute()
            .top(px(46.))
            .right(px(12.))
            .w(px(440.))
            .max_h(relative(0.85))
            .flex()
            .flex_col()
            .overflow_hidden()
            .occlude()
            .rounded_md()
            .bg(theme.panel)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            // Handled here so Esc and save act on this panel, not a line comment being edited.
            .on_action(cx.listener(|this, _: &CancelComment, window, cx| {
                this.close_finish_panel(window, cx)
            }))
            .on_action(cx.listener(|this, _: &SaveComment, window, cx| {
                this.copy_comments(&CopyComments, window, cx)
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child("Finish review"))
                    .child(
                        div()
                            .min_h(px(80.))
                            .p_2()
                            .rounded_sm()
                            .bg(theme.background)
                            .border_1()
                            .border_color(theme.accent)
                            .text_sm()
                            .line_height(px(20.))
                            .child(input.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Self::button("copy-review", "Copy review", theme)
                                    .debug_selector(|| "copy-review".to_string())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.copy_comments(&CopyComments, window, cx)
                                    })),
                            )
                            .child(div().text_xs().text_color(theme.muted).child(hint)),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted)
                    .child(format!(
                        "{count} line comment{}",
                        if count == 1 { "" } else { "s" }
                    )),
            )
            .child(
                div()
                    .id("comment-summary")
                    .flex()
                    .flex_col()
                    .min_h(px(0.))
                    .pb_1()
                    .overflow_y_scroll()
                    .children(entries),
            )
    }

    fn render_base_panel(
        &self,
        panel: &BasePanel,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hint = if cfg!(target_os = "macos") {
            "↵ to apply · Esc to close"
        } else {
            "Enter to apply · Esc to close"
        };
        div()
            .absolute()
            .top(px(46.))
            .left(px(12.))
            .w(px(440.))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .occlude()
            .rounded_md()
            .bg(theme.panel)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .on_action(
                cx.listener(|this, _: &CancelComment, window, cx| {
                    this.close_base_panel(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SaveComment, window, cx| this.apply_base(window, cx)))
            .child(div().text_sm().child("Review base"))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted)
                    .child("Arguments for git diff, like main...HEAD or --cached"),
            )
            .child(
                div()
                    .p_2()
                    .rounded_sm()
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.accent)
                    .font_family(MONO_FONT)
                    .text_xs()
                    .line_height(px(20.))
                    .child(panel.input.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .children(panel.options.iter().enumerate().map(|(ix, option)| {
                        let args = option.args.clone();
                        let is_current = option.args == self.args;
                        let hover = theme.hover;
                        div()
                            .id(("base-option", ix))
                            .debug_selector(|| format!("base-option-{ix}"))
                            .flex()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_sm()
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.load(args.clone(), window, cx)
                            }))
                            .child(
                                div()
                                    .w(px(12.))
                                    .flex_shrink_0()
                                    .text_color(theme.accent)
                                    .child(if is_current { "✓" } else { "" }),
                            )
                            .child(option.label.clone())
                    })),
            )
            .children(panel.error.clone().map(|error| {
                div()
                    .debug_selector(|| "base-error".to_string())
                    .text_xs()
                    .text_color(theme.removed)
                    .child(error)
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Self::button("apply-base", "Apply", theme)
                            .debug_selector(|| "apply-base".to_string())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.apply_base(window, cx)),
                            ),
                    )
                    .child(div().text_xs().text_color(theme.muted).child(hint)),
            )
    }

    fn render_lost_comments_dialog(
        &self,
        update: &PendingUpdate,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = update.carryover.lost.len();
        let (title, remove_label) = if count == 1 {
            (
                "1 comment isn't on a line in the new diff".to_string(),
                "Remove it and continue".to_string(),
            )
        } else {
            (
                format!("{count} comments aren't on lines in the new diff"),
                format!("Remove {count} comments and continue"),
            )
        };
        let items = update.carryover.lost.iter().map(|lost| {
            div()
                .flex_shrink_0()
                .px_3()
                .py_1p5()
                .child(
                    div()
                        .truncate()
                        .font_family(MONO_FONT)
                        .text_xs()
                        .text_color(theme.muted)
                        .child(lost.location.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .font_family(MONO_FONT)
                        .text_xs()
                        .child(lost.code.clone()),
                )
                .child(div().text_sm().child(lost.comment.clone()))
        });
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(80.))
            .child(
                div()
                    .w(px(520.))
                    .max_h(relative(0.75))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded_md()
                    .bg(theme.panel)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_lg()
                    .child(
                        div()
                            .flex_shrink_0()
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(div().text_sm().child(title))
                            .child(div().text_xs().text_color(theme.muted).child(
                                "Cancel to stay on the current diff, where you can still copy them.",
                            )),
                    )
                    .child(
                        div()
                            .id("lost-comments")
                            .flex()
                            .flex_col()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .children(items),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .p_3()
                            .flex()
                            .gap_2()
                            .border_t_1()
                            .border_color(theme.border)
                            .child(
                                Self::button("remove-lost", remove_label, theme)
                                    .debug_selector(|| "remove-lost".to_string())
                                    .text_color(theme.removed)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.accept_pending_update(window, cx)
                                    })),
                            )
                            .child(
                                Self::button("cancel-update", "Cancel", theme)
                                    .debug_selector(|| "cancel-update".to_string())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_comment(&CancelComment, window, cx)
                                    })),
                            ),
                    ),
            )
    }

    fn annotation_card(&self, theme: &Theme) -> gpui::Div {
        let indent = match self.layout {
            Layout::SideBySide => SIDE_COMMENT_INDENT,
            Layout::Inline => INLINE_COMMENT_INDENT,
        };
        div()
            .my_1()
            .ml(px(indent))
            .mr_3()
            .p_2()
            .rounded_md()
            .bg(theme.comment_bg)
            .border_1()
            .border_color(theme.border)
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        div()
            .key_context("DiffView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::copy_comments))
            .on_action(cx.listener(Self::save_comment))
            .on_action(cx.listener(Self::cancel_comment))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::toggle_layout))
            .on_action(
                cx.listener(|this, _: &ShowFiles, window, cx| {
                    this.show_tab(Tab::Files, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ShowCommits, window, cx| {
                this.show_tab(Tab::Commits, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &NextCommit, window, cx| this.step_commit(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PreviousCommit, window, cx| {
                    this.step_commit(-1, window, cx)
                }),
            )
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.text)
            .child(self.render_toolbar(&theme, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_sidebar(&theme, cx))
                    .child(self.render_main(&theme, cx)),
            )
            .children(
                self.finish_panel
                    .as_ref()
                    .map(|input| self.render_finish_panel(input, &theme, cx)),
            )
            .children(
                self.base_panel
                    .as_ref()
                    .map(|panel| self.render_base_panel(panel, &theme, cx)),
            )
            .children(
                self.pending_update
                    .as_ref()
                    .map(|update| self.render_lost_comments_dialog(update, &theme, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Diff;
    use gpui::{
        EntityInputHandler, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext,
        VisualTestContext,
    };

    const PATCH: &str = "diff --git a/a.rs b/a.rs\n\
                         --- a/a.rs\n\
                         +++ b/a.rs\n\
                         @@ -1,2 +1,2 @@\n\
                         \x20keep\n\
                         -old\n\
                         +new\n\
                         diff --git a/b.rs b/b.rs\n\
                         --- a/b.rs\n\
                         +++ b/b.rs\n\
                         @@ -1 +1 @@\n\
                         -gone\n\
                         +here\n";
    const KEEP_ROW: usize = 2;
    const NEW_ROW: usize = 4;

    fn open(cx: &mut TestAppContext) -> (Entity<DiffView>, &mut VisualTestContext) {
        open_patch(PATCH.to_string(), cx)
    }

    fn long_patch() -> String {
        let lines = "+added\n".repeat(200);
        let long_file = |name| format!("diff --git a/{name} b/{name}\n@@ -0,0 +1,200 @@\n{lines}");
        format!("{}{PATCH}{}", long_file("first.rs"), long_file("last.rs"))
    }

    fn open_patch(
        patch: String,
        cx: &mut TestAppContext,
    ) -> (Entity<DiffView>, &mut VisualTestContext) {
        cx.update(|cx| {
            TextArea::bind_keys(cx);
            DiffView::bind_keys(cx);
        });
        let review = Review::new("git diff HEAD".to_string(), Diff::parse(&patch));
        let repo = Repo::at(&std::env::temp_dir());
        let (view, cx) =
            cx.add_window_view(|window, cx| DiffView::new(review, repo, Vec::new(), window, cx));
        cx.run_until_parked();
        (view, cx)
    }

    fn click_row(view: &Entity<DiffView>, row: usize, cx: &mut VisualTestContext) {
        let (bounds, is_added) = view.read_with(cx, |view, _| {
            let bounds = view.list.bounds_for_item(view.display.item_of(row));
            let is_added = matches!(
                &view.review.diff.rows[row],
                Row::Line { line, .. } if matches!(line.kind, LineKind::Added { .. })
            );
            (bounds.expect("row is laid out"), is_added)
        });
        let column = if is_added { 0.75 } else { 0.25 };
        let position = bounds.origin + gpui::point(bounds.size.width * column, px(10.));
        cx.simulate_click(position, Modifiers::none());
        cx.run_until_parked();
    }

    fn comment(view: &Entity<DiffView>, row: usize, cx: &mut VisualTestContext) -> Option<String> {
        view.read_with(cx, |view, _| view.review.comments.get(&row).cloned())
    }

    fn draft_text(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> String {
        view.read_with(cx, |view, cx| {
            let draft = view.draft.as_ref().expect("draft is open");
            draft.input.read(cx).text().to_string()
        })
    }

    #[gpui::test]
    fn clicking_a_line_then_saving_adds_a_comment(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("first");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("second");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            comment(&view, NEW_ROW, cx).as_deref(),
            Some("first\nsecond")
        );
        assert!(view.read_with(cx, |view, _| view.draft.is_none()));
    }

    #[gpui::test]
    fn escape_discards_the_draft(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("nope");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(comment(&view, NEW_ROW, cx), None);
        assert!(view.read_with(cx, |view, _| view.draft.is_none()));
    }

    #[gpui::test]
    fn clicking_another_line_keeps_the_draft(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("keep me");
        click_row(&view, KEEP_ROW, cx);
        assert_eq!(comment(&view, NEW_ROW, cx).as_deref(), Some("keep me"));
        assert_eq!(draft_text(&view, cx), "");
    }

    #[gpui::test]
    fn reopening_a_comment_edits_it(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("draft");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        click_row(&view, NEW_ROW, cx);
        assert_eq!(draft_text(&view, cx), "draft");
        cx.simulate_input(" two");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(comment(&view, NEW_ROW, cx).as_deref(), Some("draft two"));
    }

    #[gpui::test]
    fn copy_shortcut_exports_open_draft_too(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, KEEP_ROW, cx);
        cx.simulate_input("saved");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("still typing");
        cx.simulate_keystrokes("cmd-shift-c");
        cx.run_until_parked();
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            clipboard.as_deref(),
            Some(
                "Review comments on `git diff HEAD`:\n\
                 \n## a.rs:1\n```diff\n keep\n```\nsaved\n\
                 \n## a.rs:2\n```diff\n+new\n```\nstill typing\n"
            )
        );
    }

    #[gpui::test]
    fn copy_with_no_comments_leaves_the_clipboard_alone(cx: &mut TestAppContext) {
        let (_view, cx) = open(cx);
        cx.write_to_clipboard(ClipboardItem::new_string("mine".to_string()));
        cx.simulate_keystrokes("cmd-shift-c");
        cx.run_until_parked();
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(clipboard.as_deref(), Some("mine"));
    }

    #[gpui::test]
    fn text_area_editing_keys(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);

        cx.simulate_input("helo");
        cx.simulate_keystrokes("left");
        cx.simulate_input("l");
        assert_eq!(draft_text(&view, cx), "hello");

        cx.simulate_keystrokes("cmd-right shift-enter");
        cx.simulate_input("second line");
        cx.simulate_keystrokes("up");
        cx.simulate_input("^");
        cx.simulate_keystrokes("down cmd-right backspace");
        assert_eq!(draft_text(&view, cx).lines().last(), Some("second lin"));

        cx.simulate_keystrokes("alt-backspace");
        assert_eq!(draft_text(&view, cx).lines().last(), Some("second "));

        cx.simulate_keystrokes("cmd-a cmd-x");
        assert_eq!(draft_text(&view, cx), "");
        cx.simulate_keystrokes("cmd-v cmd-v");
        let once = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap();
        assert_eq!(draft_text(&view, cx), format!("{once}{once}"));
        assert!(once.contains('^'));
    }

    #[gpui::test]
    fn sidebar_click_scrolls_to_the_file(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        let second_item = gpui::point(px(100.), px(40. + 4. + 30. + 15.));
        cx.simulate_click(second_item, Modifiers::none());
        cx.run_until_parked();
        let (top, first_row) = view.read_with(cx, |view, _| {
            (
                view.list.logical_scroll_top().item_ix,
                view.review.diff.files[1].first_row,
            )
        });
        assert_eq!(top, first_row);
    }

    fn click_selector(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector).expect("element is painted");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn panel_is_open(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> bool {
        view.read_with(cx, |view, _| view.finish_panel.is_some())
    }

    fn clipboard(cx: &mut VisualTestContext) -> Option<String> {
        cx.read_from_clipboard().and_then(|item| item.text())
    }

    #[gpui::test]
    fn finish_panel_copies_overall_comment_with_line_comments(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("line note");
        click_selector("finish-review", cx);
        assert_eq!(comment(&view, NEW_ROW, cx).as_deref(), Some("line note"));
        cx.simulate_input("ship it");
        click_selector("copy-review", cx);
        assert!(!panel_is_open(&view, cx));
        assert_eq!(
            clipboard(cx).as_deref(),
            Some(
                "Review comments on `git diff HEAD`:\n\
                 \n## Overall\nship it\n\
                 \n## a.rs:2\n```diff\n+new\n```\nline note\n"
            )
        );
    }

    #[gpui::test]
    fn overall_comment_alone_can_be_copied(cx: &mut TestAppContext) {
        let (_view, cx) = open(cx);
        click_selector("finish-review", cx);
        cx.simulate_input("all good");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            clipboard(cx).as_deref(),
            Some("Review comments on `git diff HEAD`:\n\n## Overall\nall good\n")
        );
    }

    #[gpui::test]
    fn closing_the_panel_keeps_the_overall_comment(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_selector("finish-review", cx);
        cx.simulate_input("keep me");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!panel_is_open(&view, cx));
        click_selector("finish-review", cx);
        let text = view.read_with(cx, |view, cx| {
            view.finish_panel
                .as_ref()
                .unwrap()
                .read(cx)
                .text()
                .to_string()
        });
        assert_eq!(text, "keep me");
    }

    #[gpui::test]
    fn escape_closes_the_focused_editor_first(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_selector("finish-review", cx);
        click_row(&view, KEEP_ROW, cx);
        cx.simulate_input("discard");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(comment(&view, KEEP_ROW, cx), None);
        assert!(panel_is_open(&view, cx));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!panel_is_open(&view, cx));
    }

    #[gpui::test]
    fn summary_click_scrolls_to_the_comment_without_opening_it(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        let row = 150;
        view.update(cx, |view, _| view.review.set_comment(row, "far down"));
        click_selector("finish-review", cx);
        click_selector("summary-150", cx);
        let (top, has_draft) = view.read_with(cx, |view, _| {
            (view.list.logical_scroll_top().item_ix, view.draft.is_some())
        });
        assert_eq!(top, row - REVEAL_CONTEXT_ITEMS);
        assert!(!has_draft);
    }

    #[gpui::test]
    fn summary_list_scrolls_inside_the_panel(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        view.update(cx, |view, _| {
            for row in 2..=120 {
                view.review.set_comment(row, "note");
            }
        });
        click_selector("finish-review", cx);
        let before = cx
            .debug_bounds("summary-2")
            .expect("first entry is painted");
        cx.simulate_event(ScrollWheelEvent {
            position: before.center(),
            delta: ScrollDelta::Pixels(gpui::point(px(0.), px(-300.))),
            ..Default::default()
        });
        cx.run_until_parked();
        let after = cx
            .debug_bounds("summary-2")
            .expect("first entry is painted");
        assert_eq!(after.origin.y, before.origin.y - px(300.));
        let list_top = view.read_with(cx, |view, _| view.list.logical_scroll_top().item_ix);
        assert_eq!(list_top, 0);
    }

    #[gpui::test]
    fn secondary_enter_also_saves(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("note");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert_eq!(comment(&view, NEW_ROW, cx).as_deref(), Some("note"));
    }

    #[gpui::test]
    fn reviewing_a_file_hides_its_body_and_keeps_later_rows_addressable(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let later_row = 8;
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("kept in the export");
        click_selector("reviewed-0", cx);

        assert_eq!(
            comment(&view, NEW_ROW, cx).as_deref(),
            Some("kept in the export")
        );
        let (shown_after_header, reviewed) = view.read_with(cx, |view, _| {
            (view.display.first_row_at(1), view.review.reviewed.len())
        });
        assert_eq!(reviewed, 1);
        assert_eq!(shown_after_header, Some(5));

        click_row(&view, later_row, cx);
        cx.simulate_input("later");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(comment(&view, later_row, cx).as_deref(), Some("later"));

        click_selector("reviewed-0", cx);
        let restored = view.read_with(cx, |view, _| view.display.first_row_at(1));
        assert_eq!(restored, Some(1));
    }

    #[gpui::test]
    fn reviewing_while_scrolled_into_a_file_returns_to_its_header(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        view.update(cx, |view, cx| view.jump_to_row(100, cx));
        cx.run_until_parked();
        toggle_reviewed(&view, 0, cx);
        let top = view.read_with(cx, |view, _| view.list.logical_scroll_top().item_ix);
        assert_eq!(top, 0);
    }

    fn toggle_reviewed(view: &Entity<DiffView>, file: usize, cx: &mut VisualTestContext) {
        view.update_in(cx, |view, window, cx| {
            view.toggle_reviewed(file, window, cx)
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn summary_click_opens_a_collapsed_file_without_unmarking_it(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("note");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        click_selector("reviewed-0", cx);
        click_selector("finish-review", cx);
        click_selector("summary-4", cx);
        let (reviewed, collapsed) = view.read_with(cx, |view, _| {
            (
                view.review.reviewed.contains(&0),
                view.review.collapsed.contains(&0),
            )
        });
        assert!(reviewed);
        assert!(!collapsed);
    }

    #[gpui::test]
    fn clicking_a_header_collapses_without_marking_reviewed(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_selector("file-header-0", cx);
        let state = |view: &DiffView| {
            (
                view.review.collapsed.contains(&0),
                view.review.reviewed.contains(&0),
                view.display.first_row_at(1),
            )
        };
        assert_eq!(
            view.read_with(cx, |view, _| state(view)),
            (true, false, Some(5))
        );
        click_selector("file-header-0", cx);
        assert_eq!(
            view.read_with(cx, |view, _| state(view)),
            (false, false, Some(1))
        );
    }

    #[gpui::test]
    fn reviewed_checkbox_collapses_once_and_header_can_reopen_it(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let state = |view: &DiffView| {
            (
                view.review.reviewed.contains(&0),
                view.review.collapsed.contains(&0),
            )
        };
        click_selector("reviewed-0", cx);
        assert_eq!(view.read_with(cx, |view, _| state(view)), (true, true));
        click_selector("file-header-0", cx);
        assert_eq!(view.read_with(cx, |view, _| state(view)), (true, false));
        click_selector("reviewed-0", cx);
        assert_eq!(view.read_with(cx, |view, _| state(view)), (false, false));
    }

    #[gpui::test]
    fn typing_after_clicking_an_empty_comment_box_does_not_panic(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        let input = cx.debug_bounds("comment-input").expect("editor is painted");
        let on_placeholder_text = input.origin + gpui::point(px(300.), px(14.));
        cx.simulate_click(on_placeholder_text, Modifiers::none());
        cx.run_until_parked();
        cx.simulate_input("x");
        assert_eq!(draft_text(&view, cx), "x");
    }

    /// A throwaway repository on disk, so reloads run real git.
    struct TestRepo {
        path: std::path::PathBuf,
    }

    impl TestRepo {
        fn new(name: &str, content: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("diffront-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            let repo = Self { path };
            repo.git(&["init", "-q", "-b", "main"]);
            repo.write(content);
            repo.git(&["add", "-A"]);
            repo.git(&["commit", "-qm", "base"]);
            repo
        }

        fn git(&self, args: &[&str]) {
            let status = std::process::Command::new("git")
                .current_dir(&self.path)
                .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }

        fn write(&self, content: &str) {
            std::fs::write(self.path.join("a.txt"), content).unwrap();
        }

        fn open<'a>(
            &self,
            cx: &'a mut TestAppContext,
        ) -> (Entity<DiffView>, &'a mut VisualTestContext) {
            cx.update(|cx| {
                TextArea::bind_keys(cx);
                DiffView::bind_keys(cx);
            });
            let repo = Repo::at(&self.path);
            let diff = Diff::parse(&repo.patch(&[]).unwrap());
            let review = Review::new(Repo::describe(&[]), diff);
            let (view, cx) = cx
                .add_window_view(|window, cx| DiffView::new(review, repo, Vec::new(), window, cx));
            cx.run_until_parked();
            (view, cx)
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn row_with_text(view: &Entity<DiffView>, text: &str, cx: &mut VisualTestContext) -> usize {
        view.read_with(cx, |view, _| {
            view.review
                .diff
                .rows
                .iter()
                .position(|row| matches!(row, Row::Line { line, .. } if line.text == text))
                .unwrap()
        })
    }

    fn comment_on(view: &Entity<DiffView>, text: &str, note: &str, cx: &mut VisualTestContext) {
        let row = row_with_text(view, text, cx);
        view.update(cx, |view, _| view.review.set_comment(row, note));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn refresh_keeps_comments_on_lines_that_moved(cx: &mut TestAppContext) {
        let repo = TestRepo::new("refresh-moved", "one\ntwo\n");
        repo.write("one\ntwo\nthree\n");
        let (view, cx) = repo.open(cx);
        comment_on(&view, "three", "note", cx);

        repo.write("inserted\none\ntwo\nthree\n");
        cx.simulate_keystrokes("cmd-r");
        cx.run_until_parked();

        let row = row_with_text(&view, "three", cx);
        assert_eq!(comment(&view, row, cx).as_deref(), Some("note"));
        assert!(view.read_with(cx, |view, _| view.pending_update.is_none()));
    }

    #[gpui::test]
    fn refresh_asks_before_dropping_a_comment(cx: &mut TestAppContext) {
        let repo = TestRepo::new("refresh-lost", "one\n");
        repo.write("one\ntwo\nthree\n");
        let (view, cx) = repo.open(cx);
        comment_on(&view, "two", "lost", cx);
        comment_on(&view, "three", "kept", cx);

        repo.write("one\nthree\n");
        click_selector("refresh", cx);
        assert!(view.read_with(cx, |view, _| view.pending_update.is_some()));

        // Cancelling leaves the old diff and every comment in place.
        click_selector("cancel-update", cx);
        assert!(view.read_with(cx, |view, _| view.pending_update.is_none()));
        let old_row = row_with_text(&view, "two", cx);
        assert_eq!(comment(&view, old_row, cx).as_deref(), Some("lost"));

        click_selector("refresh", cx);
        click_selector("remove-lost", cx);
        let kept = row_with_text(&view, "three", cx);
        assert_eq!(comment(&view, kept, cx).as_deref(), Some("kept"));
        assert_eq!(view.read_with(cx, |view, _| view.review.comments.len()), 1);
    }

    #[gpui::test]
    fn changing_the_base_reloads_and_updates_the_label(cx: &mut TestAppContext) {
        let repo = TestRepo::new("base", "one\n");
        repo.write("one\ntwo\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-qm", "second"]);
        repo.write("one\ntwo\nthree\n");
        let (view, cx) = repo.open(cx);
        comment_on(&view, "three", "note", cx);

        click_selector("change-base", cx);
        cx.simulate_input("HEAD~1");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let (source, panel_open) = view.read_with(cx, |view, _| {
            (view.review.source.clone(), view.base_panel.is_some())
        });
        assert_eq!(source, "git diff HEAD~1");
        assert!(!panel_open);
        let row = row_with_text(&view, "three", cx);
        assert_eq!(comment(&view, row, cx).as_deref(), Some("note"));
        row_with_text(&view, "two", cx);
    }

    #[gpui::test]
    fn invalid_base_shows_the_error_and_changes_nothing(cx: &mut TestAppContext) {
        let repo = TestRepo::new("bad-base", "one\n");
        repo.write("one\ntwo\n");
        let (view, cx) = repo.open(cx);
        comment_on(&view, "two", "note", cx);

        click_selector("change-base", cx);
        cx.simulate_input("no-such-ref");
        click_selector("apply-base", cx);

        assert!(cx.debug_bounds("base-error").is_some());
        let row = row_with_text(&view, "two", cx);
        assert_eq!(comment(&view, row, cx).as_deref(), Some("note"));
        assert_eq!(
            view.read_with(cx, |view, _| view.args.clone()),
            Vec::<String>::new()
        );
    }

    #[gpui::test]
    fn refresh_keeps_reviewed_marks_only_on_unchanged_files(cx: &mut TestAppContext) {
        let repo = TestRepo::new("reviewed", "one\n");
        repo.write("one\ntwo\n");
        std::fs::write(repo.path.join("b.txt"), "other\n").unwrap();
        let (view, cx) = repo.open(cx);
        view.update_in(cx, |view, window, cx| {
            view.toggle_reviewed(0, window, cx);
            view.toggle_reviewed(1, window, cx);
        });
        cx.run_until_parked();

        repo.write("one\ntwo\nthree\n");
        cx.simulate_keystrokes("cmd-r");
        cx.run_until_parked();

        let (reviewed, path) = view.read_with(cx, |view, _| {
            let reviewed: Vec<usize> = view.review.reviewed.iter().copied().collect();
            let path = view.review.diff.files[reviewed[0]].path.clone();
            (reviewed, path)
        });
        assert_eq!(reviewed.len(), 1);
        assert_eq!(path, "b.txt");
        row_with_text(&view, "three", cx);
    }

    #[gpui::test]
    fn each_column_comments_on_its_own_line(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let old_row = 3;
        click_row(&view, old_row, cx);
        cx.simulate_input("on the old side");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("on the new side");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            comment(&view, old_row, cx).as_deref(),
            Some("on the old side")
        );
        assert_eq!(
            comment(&view, NEW_ROW, cx).as_deref(),
            Some("on the new side")
        );
    }

    #[gpui::test]
    fn toggling_layout_keeps_comments_and_scroll_position(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        let row = 150;
        view.update(cx, |view, cx| {
            view.review.set_comment(row, "note");
            view.jump_to_row(row, cx);
        });
        cx.run_until_parked();

        click_selector("toggle-layout", cx);
        let (layout, top) = view.read_with(cx, |view, _| (view.layout, view.top_row()));
        assert_eq!(layout, Layout::Inline);
        assert_eq!(top, Some(row));
        assert_eq!(comment(&view, row, cx).as_deref(), Some("note"));

        cx.simulate_keystrokes("cmd-shift-d");
        cx.run_until_parked();
        let (layout, top) = view.read_with(cx, |view, _| (view.layout, view.top_row()));
        assert_eq!(layout, Layout::SideBySide);
        assert_eq!(top, Some(row));
    }

    #[gpui::test]
    fn lines_can_be_commented_in_the_inline_layout(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_selector("toggle-layout", cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("inline note");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(comment(&view, NEW_ROW, cx).as_deref(), Some("inline note"));
    }

    #[gpui::test]
    fn draft_survives_a_layout_toggle(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("still typing");
        cx.simulate_keystrokes("cmd-shift-d");
        cx.run_until_parked();
        cx.simulate_input(" more");
        assert_eq!(draft_text(&view, cx), "still typing more");
    }

    #[gpui::test]
    fn clicking_a_base_option_loads_that_diff(cx: &mut TestAppContext) {
        let repo = TestRepo::new("base-options", "one\n");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.write("one\ncommitted\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-qm", "on feature"]);
        repo.write("one\ncommitted\nuncommitted\n");
        let (view, cx) = repo.open(cx);
        comment_on(&view, "uncommitted", "note", cx);

        click_selector("change-base", cx);
        click_selector("base-option-0", cx);

        let (source, panel_open) = view.read_with(cx, |view, _| {
            (view.review.source.clone(), view.base_panel.is_some())
        });
        assert_eq!(source, "git diff --merge-base main");
        assert!(!panel_open);
        row_with_text(&view, "committed", cx);
        let row = row_with_text(&view, "uncommitted", cx);
        assert_eq!(comment(&view, row, cx).as_deref(), Some("note"));
    }

    fn draft_input(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> Entity<TextArea> {
        view.read_with(cx, |view, _| view.draft.as_ref().unwrap().input.clone())
    }

    #[gpui::test]
    fn system_hit_test_on_a_placeholder_returns_the_start(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        let input = draft_input(&view, cx);
        let box_bounds = cx.debug_bounds("comment-input").expect("editor is painted");
        let on_placeholder_text = box_bounds.origin + gpui::point(px(300.), px(14.));
        let index = input.update_in(cx, |input, window, cx| {
            input.character_index_for_point(on_placeholder_text, window, cx)
        });
        assert_eq!(index, Some(0));
    }

    #[gpui::test]
    fn system_hit_test_right_after_clearing_text_does_not_panic(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("some longer text");
        cx.run_until_parked();
        let input = draft_input(&view, cx);
        let box_bounds = cx.debug_bounds("comment-input").expect("editor is painted");
        let on_old_text = box_bounds.origin + gpui::point(px(60.), px(14.));
        // No frame runs between these calls, so the layout still describes the old text.
        let index = input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(Some(0..16), "", window, cx);
            input.character_index_for_point(on_old_text, window, cx)
        });
        assert_eq!(index, Some(0));
    }

    #[gpui::test]
    fn composing_after_existing_text_keeps_a_valid_cursor(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);
        cx.simulate_input("abc");
        let input = draft_input(&view, cx);
        input.update_in(cx, |input, window, cx| {
            input.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
            input.unmark_text(window, cx);
            input.replace_text_in_range(None, "!", window, cx);
        });
        assert_eq!(draft_text(&view, cx), "abcに!");
    }

    /// A feature branch with one line added per commit, plus an uncommitted line.
    fn feature_repo(name: &str) -> TestRepo {
        let repo = TestRepo::new(name, "one\n");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        for (content, subject) in [("one\nalpha\n", "alpha"), ("one\nalpha\nbeta\n", "beta")] {
            repo.write(content);
            repo.git(&["commit", "-qam", subject]);
        }
        repo.write("one\nalpha\nbeta\nuncommitted\n");
        repo
    }

    fn source(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> String {
        view.read_with(cx, |view, _| view.review.source.clone())
    }

    #[gpui::test]
    fn commits_tab_steps_through_each_commits_own_diff(cx: &mut TestAppContext) {
        let repo = feature_repo("commit-nav");
        let (view, cx) = repo.open(cx);
        click_selector("tab-commits", cx);
        assert!(source(&view, cx).ends_with("(beta)"));
        row_with_text(&view, "beta", cx);

        click_selector("next-commit", cx);
        assert!(source(&view, cx).ends_with("(alpha)"));
        row_with_text(&view, "alpha", cx);

        cx.simulate_keystrokes("cmd-[");
        cx.run_until_parked();
        assert!(source(&view, cx).ends_with("(beta)"));

        click_selector("tab-files", cx);
        assert_eq!(source(&view, cx), "git diff HEAD + untracked files");
    }

    #[gpui::test]
    fn comments_stay_with_their_view_and_all_get_exported(cx: &mut TestAppContext) {
        let repo = feature_repo("commit-comments");
        let (view, cx) = repo.open(cx);
        comment_on(&view, "uncommitted", "on the diff", cx);
        cx.simulate_keystrokes("cmd-2");
        cx.run_until_parked();
        comment_on(&view, "beta", "on a commit", cx);
        let commit_row = row_with_text(&view, "beta", cx);

        click_selector("next-commit", cx);
        assert_eq!(view.read_with(cx, |view, _| view.review.comments.len()), 0);
        click_selector("previous-commit", cx);
        assert_eq!(
            comment(&view, commit_row, cx).as_deref(),
            Some("on a commit")
        );
        assert_eq!(view.read_with(cx, |view, _| view.comment_count()), 2);

        cx.simulate_keystrokes("cmd-shift-c");
        cx.run_until_parked();
        let exported = clipboard(cx).unwrap();
        let (files, commit) = exported.split_once("Review comments on `commit ").unwrap();
        assert!(files.contains("on the diff"));
        assert!(commit.contains("(beta)`"));
        assert!(commit.contains("on a commit"));
    }

    #[gpui::test]
    fn summary_click_opens_a_comment_on_another_tab(cx: &mut TestAppContext) {
        let repo = feature_repo("commit-summary");
        let (view, cx) = repo.open(cx);
        click_selector("tab-commits", cx);
        comment_on(&view, "beta", "on a commit", cx);
        let (row, short) = view.read_with(cx, |view, _| {
            let row = *view.review.comments.keys().next().unwrap();
            (row, view.commits[0].short.clone())
        });
        click_selector("tab-files", cx);
        click_selector("finish-review", cx);
        let selector: &'static str = format!("summary-{short} {row}").leak();
        click_selector(selector, cx);
        assert!(view.read_with(cx, |view, _| view.tab == Tab::Commits));
        assert!(source(&view, cx).ends_with("(beta)"));
    }

    #[gpui::test]
    fn rewritten_commit_with_comments_is_kept_and_flagged(cx: &mut TestAppContext) {
        let repo = feature_repo("commit-amend");
        let (view, cx) = repo.open(cx);
        click_selector("tab-commits", cx);
        comment_on(&view, "beta", "still here", cx);

        repo.git(&["commit", "-q", "--amend", "-m", "beta reworded"]);
        cx.simulate_keystrokes("cmd-r");
        cx.run_until_parked();

        let commits = view.read_with(cx, |view, _| view.commits.clone());
        assert_eq!(commits[0].subject, "beta reworded");
        let kept = commits
            .iter()
            .find(|commit| commit.subject == "beta")
            .unwrap();
        assert!(!kept.in_range);
        assert_eq!(view.read_with(cx, |view, _| view.comment_count()), 1);
    }

    #[gpui::test]
    fn changing_the_base_from_commits_returns_to_files_and_updates_the_list(
        cx: &mut TestAppContext,
    ) {
        let repo = feature_repo("commit-base");
        let (view, cx) = repo.open(cx);
        click_selector("tab-commits", cx);
        assert_eq!(view.read_with(cx, |view, _| view.commits.len()), 2);

        click_selector("change-base", cx);
        cx.simulate_input("HEAD~1");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(view.read_with(cx, |view, _| view.tab == Tab::Files));
        assert_eq!(source(&view, cx), "git diff HEAD~1");
        assert_eq!(view.read_with(cx, |view, _| view.commits.len()), 1);
    }

    #[gpui::test]
    fn draft_on_a_commit_is_saved_when_navigating_away(cx: &mut TestAppContext) {
        let repo = feature_repo("commit-draft");
        let (view, cx) = repo.open(cx);
        click_selector("tab-commits", cx);
        let row = row_with_text(&view, "beta", cx);
        click_row(&view, row, cx);
        cx.simulate_input("half typed");
        click_selector("next-commit", cx);
        click_selector("previous-commit", cx);
        assert_eq!(comment(&view, row, cx).as_deref(), Some("half typed"));
    }

    fn scroll_list_to(view: &Entity<DiffView>, item_ix: usize, cx: &mut VisualTestContext) {
        view.update(cx, |view, cx| {
            view.list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
        cx.run_until_parked();
    }

    fn sticky_file(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> Option<usize> {
        cx.debug_bounds("sticky-header")?;
        view.read_with(cx, |view, _| view.sticky_header().map(|(file, _)| file))
    }

    #[gpui::test]
    fn header_sticks_until_its_file_scrolls_away(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        assert_eq!(sticky_file(&view, cx), None);

        scroll_list_to(&view, 100, cx);
        assert_eq!(sticky_file(&view, cx), Some(0));

        let second_file = view.read_with(cx, |view, _| {
            view.display.item_of(view.review.diff.files[1].first_row)
        });
        scroll_list_to(&view, second_file, cx);
        assert_eq!(sticky_file(&view, cx), None);
        scroll_list_to(&view, second_file + 1, cx);
        assert_eq!(sticky_file(&view, cx), Some(1));
    }

    #[gpui::test]
    fn next_header_pushes_the_sticky_one_up(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        let second_file = view.read_with(cx, |view, _| {
            view.display.item_of(view.review.diff.files[1].first_row)
        });
        scroll_list_to(&view, second_file - 1, cx);
        let (file, push) = view.read_with(cx, |view, _| view.sticky_header().unwrap());
        assert_eq!(file, 0);
        assert!(push < px(0.));
    }

    #[gpui::test]
    fn marking_reviewed_from_the_sticky_header_lands_on_its_file(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        scroll_list_to(&view, 100, cx);
        click_selector("reviewed-0", cx);
        let (reviewed, top) = view.read_with(cx, |view, _| {
            (
                view.review.reviewed.contains(&0),
                view.list.logical_scroll_top().item_ix,
            )
        });
        assert!(reviewed);
        assert_eq!(top, 0);
        assert_eq!(sticky_file(&view, cx), None);
    }

    #[gpui::test]
    fn wheel_over_the_sticky_header_still_scrolls(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        scroll_list_to(&view, 100, cx);
        let header = cx.debug_bounds("sticky-header").unwrap();
        cx.simulate_event(ScrollWheelEvent {
            position: header.center(),
            delta: ScrollDelta::Pixels(gpui::point(px(0.), px(-200.))),
            ..Default::default()
        });
        cx.run_until_parked();
        let top = view.read_with(cx, |view, _| view.list.logical_scroll_top().item_ix);
        assert!(top > 100);
    }
}
