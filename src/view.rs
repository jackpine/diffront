use crate::diff::{Change, Content, Diff, DiffLine, LineKind, Row};
use crate::display::{Column, DisplayList, Item, Layout};
use crate::git::{BaseOption, Commit, Repo, Revision, RevisionKey};
use crate::overscroll::Overscroll;
use crate::review::{Carryover, Review};
use crate::scrollbar::Scrollbar;
use crate::search::Search;
use crate::selection::{TextSelection, Unit};
use crate::symbols::{Symbol, Symbols};
use crate::tada::{Tada, ease_out_cubic};
use crate::text_area::TextArea;
use crate::theme::{MONO_FONT, Theme};
#[cfg(target_os = "macos")]
use crate::voice::{self, VoiceCommand};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, DispatchPhase,
    Entity, FocusHandle, Focusable, HighlightStyle, Hsla, KeyBinding, KeyDownEvent, ListAlignment,
    ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString, Stateful, StyledText,
    Subscription, Task, TextLayout, TouchPhase, Window, actions, canvas, combine_highlights,
    deferred, div, img, list, prelude::*, px, relative,
};
use std::collections::BTreeMap;
use std::ops::Range;
use std::time::{Duration, Instant};

actions!(
    diffront,
    [
        CopyComments,
        CopySelection,
        SaveComment,
        CancelComment,
        Refresh,
        ToggleLayout,
        ShowFiles,
        ShowCommits,
        NextCommit,
        PreviousCommit,
        ToggleCurrentFileReviewed,
        ScrollDown,
        ScrollUp,
        ShowSearch,
        NextMatch,
        PreviousMatch
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
const KEY_SCROLL_DISTANCE: f32 = 60.;
/// The "All done!" footer after the diff's last row.
const FOOTER_ITEMS: usize = 1;
/// How far below the top of the diff the current file is read, so a just-reviewed file stays in
/// view above it.
const CURRENT_FILE_PROBE: f32 = 64.;
const GLIDE: Duration = Duration::from_millis(180);
#[cfg(target_os = "macos")]
const VOICE_SCROLL_VIEWPORT_FRACTION: f32 = 0.75;
/// Clears the sticky file header, which would otherwise cover a match at the top of the list.
const MATCH_TOP_MARGIN: f32 = 40.;
/// The height of a diff line, used to estimate the height of rows not yet laid out.
const ROW_HEIGHT: f32 = 20.;
const SCROLLBAR_WIDTH: f32 = 10.;
const SIDEBAR_WIDTH: f32 = 280.;
const MIN_SIDEBAR_WIDTH: f32 = 160.;
const MIN_MAIN_WIDTH: f32 = 320.;
const SIDEBAR_HANDLE_WIDTH: f32 = 6.;
/// Hidden lines revealed per click on a hunk header.
const EXPAND_STEP: u32 = 20;

/// A scrollable part of the window that draws its own scrollbar.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ScrollArea {
    Diff,
    Files,
    Commits,
}

impl ScrollArea {
    fn id(self) -> &'static str {
        match self {
            Self::Diff => "diff-scrollbar",
            Self::Files => "files-scrollbar",
            Self::Commits => "commits-scrollbar",
        }
    }
}

/// A quick eased scroll that brings a file header to the top, with the gap above it scrolled away.
#[derive(Clone, Copy, Debug)]
struct Glide {
    item: usize,
    stage: GlideStage,
    travelled: Pixels,
}

#[derive(Clone, Copy, Debug)]
enum GlideStage {
    /// The list hasn't been laid out since the glide was set up.
    AwaitingLayout,
    Measuring,
    Moving {
        started: Instant,
        distance: Pixels,
    },
}

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
    Revision(RevisionKey),
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

/// The file list's filter field, opened by typing while nothing else has focus.
struct FileFilter {
    input: Entity<TextArea>,
    /// Lowercased, so matching ignores case.
    query: String,
    _changes: Subscription,
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
    commits: Vec<Revision>,
    commit_scroll: ScrollHandle,
    selected_commit: Option<RevisionKey>,
    commits_load: Option<Task<()>>,
    commit_load: Option<Task<()>>,
    overall: String,
    symbols: Symbols,
    repo: Repo,
    args: Vec<String>,
    layout: Layout,
    display: DisplayList,
    list: ListState,
    file_scroll: ScrollHandle,
    /// The scrollbar being dragged, and where the pointer holds its thumb from the thumb's top.
    scrollbar_grab: Option<(ScrollArea, Pixels)>,
    sidebar_width: Pixels,
    /// While resizing the sidebar, how far the pointer is from its edge.
    sidebar_grab: Option<Pixels>,
    /// The file the review shortcut targets, as of this frame; the list can't be queried mid-render.
    shortcut_file: Option<usize>,
    overscroll: Overscroll,
    tada: Tada,
    glide: Option<Glide>,
    focus: FocusHandle,
    draft: Option<Draft>,
    selection: Option<TextSelection>,
    click_opens_draft: bool,
    search: Search,
    search_input: Entity<TextArea>,
    _search_input_changes: Subscription,
    file_filter: Option<FileFilter>,
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
        let list = ListState::new(display.len() + FOOTER_ITEMS, ListAlignment::Top, px(400.));
        let search_input = cx.new(|cx| TextArea::new(String::new(), "Find in diff", cx));
        let search_input_changes = cx.observe(&search_input, |this, _, cx| {
            if this.search.open && this.search_input.read(cx).text() != this.search.query() {
                this.run_search(cx);
            }
        });
        let mut view = Self {
            review,
            active: ViewKey::Files,
            parked: BTreeMap::new(),
            tab: Tab::Files,
            commits: Vec::new(),
            commit_scroll: ScrollHandle::new(),
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
            file_scroll: ScrollHandle::new(),
            scrollbar_grab: None,
            sidebar_width: px(SIDEBAR_WIDTH),
            sidebar_grab: None,
            shortcut_file: None,
            overscroll: Overscroll::default(),
            tada: Tada::default(),
            glide: None,
            focus,
            draft: None,
            selection: None,
            click_opens_draft: true,
            search: Search::default(),
            search_input,
            _search_input_changes: search_input_changes,
            file_filter: None,
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
            KeyBinding::new("secondary-c", CopySelection, Some("DiffView")),
            KeyBinding::new("secondary-shift-r", ToggleCurrentFileReviewed, None),
            KeyBinding::new("j", ScrollDown, Some("DiffView && !TextArea")),
            KeyBinding::new("k", ScrollUp, Some("DiffView && !TextArea")),
            KeyBinding::new("secondary-f", ShowSearch, None),
            KeyBinding::new("secondary-g", NextMatch, None),
            KeyBinding::new("secondary-shift-g", PreviousMatch, None),
            KeyBinding::new("shift-enter", PreviousMatch, Some("SearchBar > TextArea")),
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
        if let Some(draft) = self.draft.as_ref().filter(|draft| draft.row == row) {
            window.focus(&draft.input.focus_handle(cx));
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

    /// A press that only dismisses a selection shouldn't also open a comment.
    fn dismiss_selection(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.clear_selection();
        self.click_opens_draft = !dismissed;
        if dismissed {
            cx.notify();
        }
    }

    /// Returns whether any text was selected.
    fn clear_selection(&mut self) -> bool {
        self.selection.take().is_some_and(|sel| !sel.is_empty())
    }

    fn start_selection(
        &mut self,
        column: Column,
        row: usize,
        layout: &TextLayout,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let unit = Unit::for_click_count(event.click_count);
        let offset = self.text_offset(row, layout, event.position, unit);
        self.selection = Some(TextSelection::new(
            column,
            unit,
            row,
            &self.line(row).text,
            offset,
        ));
        if unit != Unit::Character {
            self.click_opens_draft = false;
            self.drop_untouched_draft(row, cx);
            cx.notify();
        }
        window.focus(&self.focus);
    }

    /// Drops the comment box the first click of a double click opened, unless it's been typed in.
    fn drop_untouched_draft(&mut self, row: usize, cx: &mut Context<Self>) {
        let saved = self
            .review
            .comments
            .get(&row)
            .map_or("", |comment| comment.as_str());
        if self
            .draft
            .as_ref()
            .is_some_and(|draft| draft.row == row && draft.input.read(cx).text() == saved)
        {
            self.draft = None;
        }
    }

    fn extend_selection(
        &mut self,
        column: Column,
        row: usize,
        layout: &TextLayout,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(unit) = self.selection.map(|sel| sel.unit()) else {
            return;
        };
        let offset = self.text_offset(row, layout, position, unit);
        // Borrows the row by field so the selection can be borrowed mutably alongside it.
        let Row::Line { line, .. } = &self.review.diff.rows[row] else {
            unreachable!("only line rows are selectable");
        };
        let Some(selection) = self.selection.as_mut().filter(|sel| sel.column == column) else {
            return;
        };
        if selection.extend(row, &line.text, offset) {
            self.click_opens_draft &= selection.is_empty();
            cx.notify();
        }
    }

    fn line(&self, row: usize) -> &DiffLine {
        let Row::Line { line, .. } = &self.review.diff.rows[row] else {
            unreachable!("only line rows are selectable");
        };
        line
    }

    /// The source offset under `position`: the nearest caret, or for words and lines the character pressed.
    fn text_offset(
        &self,
        row: usize,
        layout: &TextLayout,
        position: Point<Pixels>,
        unit: Unit,
    ) -> usize {
        let line = self.line(row);
        let mut drawn = layout
            .index_for_position(position)
            .unwrap_or_else(|closest| closest);
        let past_pointer = layout
            .position_for_index(drawn)
            .is_some_and(|caret| caret.x > position.x);
        if unit != Unit::Character && past_pointer {
            drawn = line.display_text()[..drawn]
                .char_indices()
                .next_back()
                .map_or(0, |(ix, _)| ix);
        }
        line.source_offset(drawn)
    }

    fn copy_selection(&mut self, _: &CopySelection, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(selection) = self.selection.filter(|sel| !sel.is_empty()) {
            let text = selection.text(&self.review.diff, &self.display);
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn rebuild_display(&mut self) {
        self.selection = None;
        self.display = DisplayList::new(&self.review, self.layout);
        self.search.refresh(&self.review.diff, &self.display);
    }

    /// Opens search, or returns to it, seeding the query from any single-line diff selection.
    fn show_search(&mut self, _: &ShowSearch, window: &mut Window, cx: &mut Context<Self>) {
        let seed = self
            .selection
            .filter(|sel| !sel.is_empty())
            .map(|sel| sel.text(&self.review.diff, &self.display))
            .filter(|text| !text.contains('\n'));
        if let Some(seed) = seed {
            self.selection = None;
            self.search_input
                .update(cx, |input, cx| input.set_text(&seed, cx));
        }
        self.search_input
            .update(cx, |input, cx| input.select_all(cx));
        self.search.open = true;
        self.focus_search(window, cx);
        self.run_search(cx);
    }

    /// Starts or extends the file filter with a character typed outside any text field.
    fn type_into_file_filter(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if !self.focus.is_focused(window)
            || !self.showing_diff()
            || modifiers.control
            || modifiers.platform
            || modifiers.function
        {
            return;
        }
        let Some(typed) = keystroke
            .key_char
            .as_deref()
            .filter(|typed| !typed.trim().is_empty())
        else {
            return;
        };
        cx.stop_propagation();
        if let Some(filter) = &self.file_filter {
            let input = filter.input.clone();
            input.update(cx, |input, cx| {
                let text = format!("{}{typed}", input.text());
                input.set_text(&text, cx);
            });
            window.focus(&input.focus_handle(cx));
            return;
        }
        let input = cx.new(|cx| TextArea::new(typed.to_string(), "Filter files", cx));
        let changes = cx.observe_in(&input, window, |this, input, window, cx| {
            let query = input.read(cx).text().to_lowercase();
            if query.is_empty() {
                this.close_file_filter(window, cx);
            } else if let Some(filter) = &mut this.file_filter {
                filter.query = query;
            }
            cx.notify();
        });
        window.focus(&input.focus_handle(cx));
        self.file_filter = Some(FileFilter {
            input,
            query: typed.to_lowercase(),
            _changes: changes,
        });
        cx.notify();
    }

    fn close_file_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_filter = None;
        window.focus(&self.focus);
        cx.notify();
    }

    /// Jumps to the first file the filter shows and hands keys back to the diff.
    fn open_first_filtered_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = self.filtered_files().next();
        if let Some(file) = first {
            self.jump_to_file(file, cx);
        }
        window.focus(&self.focus);
    }

    fn filtered_files(&self) -> impl Iterator<Item = usize> + '_ {
        let query = self.file_filter.as_ref().map(|filter| &filter.query);
        self.review
            .diff
            .files
            .iter()
            .enumerate()
            .filter(move |(_, file)| {
                query.is_none_or(|query| file.path.to_lowercase().contains(query.as_str()))
            })
            .map(|(ix, _)| ix)
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.close();
        window.focus(&self.focus);
        cx.notify();
    }

    fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.search_input.focus_handle(cx));
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).text().to_string();
        let top_row = self.top_row();
        self.search
            .run(query, &self.review.diff, &self.display, top_row);
        self.reveal_current_match(cx);
    }

    fn step_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.search.step(delta);
        self.reveal_current_match(cx);
    }

    fn toggle_search_case(&mut self, cx: &mut Context<Self>) {
        self.search.case_sensitive = !self.search.case_sensitive;
        self.run_search(cx);
    }

    /// Scrolls to the current match, unless it's already in clear view.
    fn reveal_current_match(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        let Some(row) = self.search.current_row() else {
            return;
        };
        let item = self.display.item_of(row);
        let viewport = self.list.viewport_bounds();
        let in_view = self.list.bounds_for_item(item).is_some_and(|bounds| {
            bounds.top() >= viewport.top() + px(MATCH_TOP_MARGIN)
                && bounds.bottom() <= viewport.bottom()
        });
        if !in_view {
            self.list.scroll_to(ListOffset {
                item_ix: item.saturating_sub(REVEAL_CONTEXT_ITEMS),
                offset_in_item: px(0.),
            });
        }
    }

    /// A row's text, with its search matches and `extra` highlighted over `underlay`.
    fn highlighted_text(
        &self,
        row: usize,
        text: impl Into<SharedString>,
        extra: Option<(Range<usize>, HighlightStyle)>,
        underlay: Vec<(Range<usize>, HighlightStyle)>,
        theme: &Theme,
    ) -> StyledText {
        let matches = self.search.matches_in(row).map(|(range, current)| {
            let style = if current {
                HighlightStyle {
                    background_color: Some(theme.search_current),
                    color: Some(theme.search_current_text),
                    ..Default::default()
                }
            } else {
                HighlightStyle {
                    background_color: Some(theme.search_match),
                    ..Default::default()
                }
            };
            (range, style)
        });
        let overlays: Vec<_> = combine_highlights(matches, extra).collect();
        // Overlapping backgrounds merge in arbitrary order, so the underlay only fills the gaps.
        let underlay = underlay.into_iter().flat_map(|(range, style)| {
            Self::uncovered(range, &overlays)
                .into_iter()
                .map(move |range| (range, style))
        });
        let underlay: Vec<_> = underlay.collect();
        let highlights: Vec<_> = combine_highlights(underlay, overlays).collect();
        StyledText::new(text).with_highlights(highlights)
    }

    /// The parts of `range` outside the sorted, disjoint `covers`.
    fn uncovered(
        range: Range<usize>,
        covers: &[(Range<usize>, HighlightStyle)],
    ) -> Vec<Range<usize>> {
        let mut gaps = Vec::new();
        let mut start = range.start;
        for (cover, _) in covers {
            if cover.start >= range.end {
                break;
            }
            if cover.start > start {
                gaps.push(start..cover.start);
            }
            start = start.max(cover.end);
        }
        if start < range.end {
            gaps.push(start..range.end);
        }
        gaps
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
        if self.pending_update.take().is_some() || self.clear_selection() {
            cx.notify();
        } else if self.draft.is_some() {
            self.close_draft(window, cx);
        } else if self.search.open {
            self.close_search(window, cx);
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
                    .map(|revision| ViewKey::Revision(revision.key())),
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
                let Some(selected) = &self.selected_commit else {
                    return false;
                };
                matches!(&self.active, ViewKey::Revision(active) if active == selected)
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
        self.rebuild_display();
        self.list.reset(self.display.len() + FOOTER_ITEMS);
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

    /// Shows the selected commit, or the newest one if it's no longer listed.
    fn select_listed_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self
            .selected_commit
            .clone()
            .filter(|key| self.position_of(key).is_some())
            .or(self.commits.last().map(Revision::key));
        match selected {
            Some(key) => self.show_commit(key, window, cx),
            None => self.selected_commit = None,
        }
    }

    fn position_of(&self, key: &RevisionKey) -> Option<usize> {
        self.commits
            .iter()
            .position(|revision| revision.key() == *key)
    }

    fn revision(&self, key: &RevisionKey) -> Option<&Revision> {
        self.commits.iter().find(|revision| revision.key() == *key)
    }

    fn select_commit(&mut self, key: RevisionKey) {
        match self.position_of(&key) {
            // Unlike scrolling to an item, this also works before the list's first layout.
            Some(ix) if ix + 1 == self.commits.len() => self.commit_scroll.scroll_to_bottom(),
            Some(ix) => self.commit_scroll.scroll_to_item(ix),
            None => {}
        }
        self.selected_commit = Some(key);
    }

    fn show_commit(&mut self, key: RevisionKey, window: &mut Window, cx: &mut Context<Self>) {
        self.save_open_draft(window, cx);
        self.select_commit(key.clone());
        let view_key = ViewKey::Revision(key.clone());
        if self.review_for(&view_key).is_some() {
            self.commit_load = None;
            self.activate(view_key, cx);
            return;
        }
        let repo = self.repo.clone();
        let patch = {
            let key = key.clone();
            cx.background_spawn(async move { repo.revision_diff(&key) })
        };
        self.commit_load = Some(cx.spawn_in(window, async move |this, cx| {
            let patch = patch.await;
            this.update_in(cx, |this, _, cx| {
                this.commit_load = None;
                this.finish_commit_load(key, patch, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    fn finish_commit_load(
        &mut self,
        key: RevisionKey,
        patch: Result<Diff, String>,
        cx: &mut Context<Self>,
    ) {
        let patch = match patch {
            Ok(patch) => patch,
            Err(error) => {
                self.show_status(format!("Couldn't load commit: {error}"), cx);
                return;
            }
        };
        let Some(source) = self.revision(&key).map(Revision::source) else {
            return;
        };
        let key = ViewKey::Revision(key);
        self.parked.insert(
            key.clone(),
            Parked {
                review: Review::new(source, patch),
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
        let target = self
            .selected_commit
            .as_ref()
            .and_then(|key| self.position_of(key))
            .and_then(|current| current.checked_add_signed(delta))
            .and_then(|target| self.commits.get(target))
            .map(Revision::key);
        if let Some(key) = target {
            self.show_commit(key, window, cx);
        }
    }

    fn reload_commits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        let args = self.args.clone();
        let loaded: Vec<RevisionKey> = [RevisionKey::Staged, RevisionKey::Unstaged]
            .into_iter()
            .filter(|key| self.review_for(&ViewKey::Revision(key.clone())).is_some())
            .collect();
        let reload = cx.background_spawn(async move {
            let revisions = repo.revisions(&args).unwrap_or_default();
            let patches: Vec<_> = loaded
                .into_iter()
                .map(|key| {
                    let patch = repo.revision_diff(&key);
                    (key, patch)
                })
                .collect();
            (revisions, patches)
        });
        self.commits_load = Some(cx.spawn_in(window, async move |this, cx| {
            let (revisions, patches) = reload.await;
            this.update_in(cx, |this, window, cx| {
                this.commits_load = None;
                this.finish_commits(revisions, patches, window, cx);
            })
            .ok();
        }));
    }

    /// Replaces the commit list, keeping commits with comments even if they left the range.
    fn finish_commits(
        &mut self,
        revisions: Vec<Revision>,
        uncommitted_patches: Vec<(RevisionKey, Result<Diff, String>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut kept = Vec::new();
        for revision in std::mem::take(&mut self.commits) {
            let Revision::Commit(commit) = revision else {
                continue;
            };
            let key = RevisionKey::Commit(commit.sha.clone());
            let listed = revisions.iter().any(|new| new.key() == key);
            let has_comments = self
                .review_for(&ViewKey::Revision(key))
                .is_some_and(|review| !review.comments.is_empty());
            if !listed && has_comments {
                kept.push(Revision::Commit(Commit {
                    in_range: false,
                    ..commit
                }));
            }
        }
        kept.extend(revisions);
        self.commits = kept;
        let lost: usize = uncommitted_patches
            .into_iter()
            .map(|(key, patch)| self.refresh_uncommitted(key, patch, cx))
            .sum();
        if self.tab == Tab::Commits {
            self.select_listed_commit(window, cx);
        }
        match lost {
            0 => {}
            1 => self.show_status("1 comment on uncommitted changes was dropped".into(), cx),
            count => self.show_status(
                format!("{count} comments on uncommitted changes were dropped"),
                cx,
            ),
        }
        cx.notify();
    }

    /// Moves an uncommitted review onto its newer diff, returning how many comments were lost.
    fn refresh_uncommitted(
        &mut self,
        key: RevisionKey,
        patch: Result<Diff, String>,
        cx: &mut Context<Self>,
    ) -> usize {
        let view_key = ViewKey::Revision(key.clone());
        let source = self.revision(&key).map(Revision::source);
        let (Some(source), Ok(patch)) = (source, patch) else {
            let lost = self
                .review_for(&view_key)
                .map_or(0, |review| review.comments.len());
            if self.active == view_key {
                self.activate(ViewKey::Files, cx);
            }
            self.parked.remove(&view_key);
            return lost;
        };
        let diff = patch;
        let is_active = self.active == view_key;
        let review = if is_active {
            &mut self.review
        } else {
            match self.parked.get_mut(&view_key) {
                Some(parked) => &mut parked.review,
                None => return 0,
            }
        };
        let carryover = review.carry_over(&diff);
        let lost = carryover.lost.len();
        review.apply_update(source, diff, carryover);
        if is_active {
            self.rebuild_display();
            self.list.reset(self.display.len() + FOOTER_ITEMS);
        }
        lost
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
            ViewKey::Revision(revision) => {
                self.tab = Tab::Commits;
                self.select_commit(revision.clone());
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
        self.rebuild_display();
        self.list.reset(self.display.len() + FOOTER_ITEMS);
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
        if matches!(row, Row::File(_)) && top.offset_in_item <= px(FILE_GAP) {
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

    fn scroll_list_by(&mut self, distance: Pixels, cx: &mut Context<Self>) {
        self.list.scroll_by(distance);
        cx.notify();
    }

    fn scroll_handle(&self, area: ScrollArea) -> Option<&ScrollHandle> {
        match area {
            ScrollArea::Diff => None,
            ScrollArea::Files => Some(&self.file_scroll),
            ScrollArea::Commits => Some(&self.commit_scroll),
        }
    }

    /// The diff scrolls by rows of estimated height; the sidebar lists by pixels.
    fn scrollbar(&self, area: ScrollArea) -> Option<Scrollbar> {
        if let Some(handle) = self.scroll_handle(area) {
            let track = handle.bounds();
            let visible = track.size.height / px(1.);
            let total = visible + handle.max_offset().height / px(1.);
            return Scrollbar::new(track, visible, total, -handle.offset().y / px(1.));
        }
        let top = self.list.logical_scroll_top();
        let item_height = self
            .list
            .bounds_for_item(top.item_ix)
            .map_or(px(ROW_HEIGHT), |bounds| bounds.size.height);
        let offset = if item_height > px(0.) {
            (top.offset_in_item / item_height).min(1.)
        } else {
            0.
        };
        let track = self.list.viewport_bounds();
        Scrollbar::new(
            track,
            track.size.height / px(ROW_HEIGHT),
            self.list.item_count() as f32,
            top.item_ix as f32 + offset,
        )
    }

    fn scroll_area_to(&self, area: ScrollArea, position: f32) {
        match self.scroll_handle(area) {
            Some(handle) => handle.set_offset(gpui::point(px(0.), -px(position))),
            None => self.list.scroll_to(ListOffset {
                item_ix: position as usize,
                offset_in_item: px(ROW_HEIGHT) * position.fract(),
            }),
        }
    }

    /// Grabs the thumb under the pointer, or centers it on the pointer when pressing the track.
    fn press_scrollbar(&mut self, area: ScrollArea, y: Pixels, cx: &mut Context<Self>) {
        let Some(scrollbar) = self.scrollbar(area) else {
            return;
        };
        let grab = if scrollbar.contains_thumb(y) {
            y - scrollbar.track.top() - scrollbar.thumb_top()
        } else {
            scrollbar.thumb_height / 2.
        };
        self.scrollbar_grab = Some((area, grab));
        self.drag_scrollbar(y, cx);
    }

    fn drag_scrollbar(&mut self, y: Pixels, cx: &mut Context<Self>) {
        let Some((area, grab)) = self.scrollbar_grab else {
            return;
        };
        let Some(scrollbar) = self.scrollbar(area) else {
            return;
        };
        self.scroll_area_to(
            area,
            scrollbar.position_at(y - scrollbar.track.top() - grab),
        );
        cx.notify();
    }

    fn release_scrollbar(&mut self, cx: &mut Context<Self>) {
        if self.scrollbar_grab.take().is_some() {
            cx.notify();
        }
    }

    fn drag_sidebar(&mut self, x: Pixels, window_width: Pixels, cx: &mut Context<Self>) {
        let Some(grab) = self.sidebar_grab else {
            return;
        };
        let max = (window_width - px(MIN_MAIN_WIDTH)).max(px(MIN_SIDEBAR_WIDTH));
        self.sidebar_width = (x - grab).clamp(px(MIN_SIDEBAR_WIDTH), max);
        cx.notify();
    }

    fn render_sidebar_handle(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let dragging = self.sidebar_grab.is_some();
        let view = cx.entity().downgrade();
        let accent = theme.accent;
        deferred(
            div()
                .id("sidebar-handle")
                .debug_selector(|| "sidebar-handle".to_string())
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(-SIDEBAR_HANDLE_WIDTH / 2. - 1.))
                .w(px(SIDEBAR_HANDLE_WIDTH))
                .occlude()
                .cursor(CursorStyle::ResizeLeftRight)
                .flex()
                .justify_center()
                .child(
                    div()
                        .w(px(2.))
                        .h_full()
                        .when(dragging, |line| line.bg(accent))
                        .hover(move |line| line.bg(accent)),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.sidebar_grab = Some(event.position.x - this.sidebar_width);
                        cx.notify();
                    }),
                )
                .when(dragging, |handle| {
                    handle.child(
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, _| {
                                let moved = view.clone();
                                window.on_mouse_event(
                                    move |event: &MouseMoveEvent, _, window, cx| {
                                        let width = window.viewport_size().width;
                                        moved
                                            .update(cx, |this, cx| {
                                                this.drag_sidebar(event.position.x, width, cx)
                                            })
                                            .ok();
                                    },
                                );
                                let released = view.clone();
                                window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                                    released
                                        .update(cx, |this, cx| {
                                            this.sidebar_grab = None;
                                            cx.notify();
                                        })
                                        .ok();
                                });
                            },
                        )
                        .size_0(),
                    )
                }),
        )
    }

    fn render_scrollbar(
        &self,
        area: ScrollArea,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let scrollbar = self.scrollbar(area)?;
        let dragging = self.scrollbar_grab.is_some_and(|(held, _)| held == area);
        let hover = theme.muted.opacity(0.8);
        let view = cx.entity().downgrade();
        Some(
            div()
                .id(area.id())
                .debug_selector(|| area.id().to_string())
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(SCROLLBAR_WIDTH))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.press_scrollbar(area, event.position.y, cx)
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .top(scrollbar.thumb_top())
                        .h(scrollbar.thumb_height)
                        .left(px(2.))
                        .right(px(2.))
                        .rounded_full()
                        .bg(if dragging {
                            hover
                        } else {
                            theme.muted.opacity(0.45)
                        })
                        .hover(move |style| style.bg(hover)),
                )
                .when(dragging, |track| {
                    track.child(
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, _| {
                                let moved = view.clone();
                                window.on_mouse_event(move |event: &MouseMoveEvent, _, _, cx| {
                                    moved
                                        .update(cx, |this, cx| {
                                            this.drag_scrollbar(event.position.y, cx)
                                        })
                                        .ok();
                                });
                                let released = view.clone();
                                window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                                    released
                                        .update(cx, |this, cx| this.release_scrollbar(cx))
                                        .ok();
                                });
                            },
                        )
                        .size_0(),
                    )
                }),
        )
    }

    #[cfg(target_os = "macos")]
    pub fn listen_for_voice_commands(&self, window: &mut Window, cx: &mut Context<Self>) {
        use futures::StreamExt;

        let (adapter, mut commands) = voice::publish(window);
        cx.spawn_in(window, async move |this, cx| {
            let _adapter = adapter;
            while let Some(command) = commands.next().await {
                let handled = this.update_in(cx, |this, window, cx| {
                    this.run_voice_command(command, window, cx)
                });
                if handled.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    #[cfg(target_os = "macos")]
    fn run_voice_command(
        &mut self,
        command: VoiceCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = self.list.viewport_bounds().size.height * VOICE_SCROLL_VIEWPORT_FRACTION;
        match command {
            VoiceCommand::MarkReviewed => {
                self.toggle_current_file_reviewed(&ToggleCurrentFileReviewed, window, cx)
            }
            VoiceCommand::PageDown => self.scroll_list_by(page, cx),
            VoiceCommand::PageUp => self.scroll_list_by(-page, cx),
        }
    }

    /// The file at the top of the list, whose header is pinned once scrolled past.
    /// The file whose header is pinned, or else the one a little below the top of the diff, under
    /// any file just reviewed and collapsed.
    fn current_file(&self) -> Option<usize> {
        if let Some((file, _)) = self.sticky_header() {
            return Some(file);
        }
        let probe = self.list.viewport_bounds().top() + px(CURRENT_FILE_PROBE);
        let top = self.list.logical_scroll_top().item_ix;
        let item = (top..self.display.len())
            .map_while(|ix| Some((ix, self.list.bounds_for_item(ix)?)))
            .find(|(_, bounds)| bounds.bottom() > probe)
            .map_or(top, |(ix, _)| ix);
        let row = self.display.first_row_at(item)?;
        Some(self.review.diff.rows[row].file())
    }

    /// Notes the file the review shortcut targets this frame, keeping it in view in the file list.
    fn follow_shortcut_file(&mut self) {
        let current = self.review_target();
        if current != self.shortcut_file
            && let Some(file) = current
            && let Some(item) = self.filtered_files().position(|shown| shown == file)
        {
            self.file_scroll.scroll_to_item(item);
        }
        self.shortcut_file = current;
    }

    fn toggle_current_file_reviewed(
        &mut self,
        _: &ToggleCurrentFileReviewed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(file) = self.review_target().filter(|_| self.showing_diff()) {
            self.toggle_reviewed(file, window, cx);
        }
    }

    /// The current file, or while gliding past a reviewed file, the one it's gliding to.
    fn review_target(&self) -> Option<usize> {
        let Some(glide) = self.glide else {
            return self.current_file();
        };
        let reviewed = self.review.diff.rows[self.display.first_row_at(glide.item)?].file();
        Some(reviewed + 1).filter(|next| *next < self.review.diff.files.len())
    }

    fn toggle_reviewed(&mut self, file: usize, window: &mut Window, cx: &mut Context<Self>) {
        let reviewed = self.review.reviewed.insert(file);
        if !reviewed {
            self.review.reviewed.remove(&file);
        }
        let pinned = self
            .sticky_header()
            .is_some_and(|(pinned, _)| pinned == file);
        let next_file_top = (reviewed && pinned).then(|| self.top_after_file(file));
        self.set_collapsed(file, reviewed, window, cx);
        if let Some(next_file_top) = next_file_top {
            self.lower_header_to(file, next_file_top);
        }
        if reviewed {
            self.advance_past(file, cx);
        }
        cx.notify();
    }

    /// Where whatever follows `file` starts on screen, at most the bottom of the diff.
    fn top_after_file(&self, file: usize) -> Pixels {
        let after = self
            .review
            .diff
            .files
            .get(file + 1)
            .map_or(self.display.len(), |next| {
                self.display.item_of(next.first_row)
            });
        let bottom = self.list.viewport_bounds().bottom();
        self.list
            .bounds_for_item(after)
            .map_or(bottom, |bounds| bounds.top().min(bottom))
    }

    /// Scrolls back so `file`'s header ends near `bottom`, for the glide to bring it up from there.
    fn lower_header_to(&mut self, file: usize, bottom: Pixels) {
        let header = self.display.item_of(self.review.diff.files[file].first_row);
        let rows_above = ((bottom - self.list.viewport_bounds().top()) / px(ROW_HEIGHT)) as usize;
        self.list.scroll_to(ListOffset {
            item_ix: header.saturating_sub(rows_above.saturating_sub(1)),
            offset_in_item: px(0.),
        });
    }

    /// Glides the just-reviewed `file` to the top, leaving the next file below it as current.
    fn advance_past(&mut self, file: usize, cx: &mut Context<Self>) {
        self.glide = Some(Glide {
            item: self.display.item_of(self.review.diff.files[file].first_row),
            stage: GlideStage::AwaitingLayout,
            travelled: px(0.),
        });
        cx.notify();
    }

    /// Reveals the lines hidden at a hunk header or file tail, a step at a time or all at once.
    fn expand(&mut self, row: usize, old_blob: &str, all: bool, cx: &mut Context<Self>) {
        let old_lines = match self.repo.blob_lines(old_blob) {
            Ok(lines) => lines,
            Err(err) => return self.show_status(format!("Couldn't read the file: {err}"), cx),
        };
        let header = self.display.item_of(row);
        let limit = if all { u32::MAX } else { EXPAND_STEP };
        let replacement = self.review.expand(row, &old_lines, limit);
        if let Some(draft) = self.draft.as_mut().filter(|draft| draft.row > row) {
            draft.row = draft.row + replacement - 1;
        }
        self.rebuild_display();
        self.list.splice(header..header + 1, replacement);
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
        self.rebuild_display();
        if collapsed {
            let hidden = first_body_item..first_body_item + (old_len - self.display.len());
            let scrolled_into_body = hidden.contains(&self.list.logical_scroll_top().item_ix);
            self.list.splice(hidden, 0);
            if scrolled_into_body {
                // Leaves the header where it was pinned, with the gap above it scrolled away.
                self.list.scroll_to(ListOffset {
                    item_ix: header,
                    offset_in_item: px(FILE_GAP),
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
            cx.background_spawn(async move { repo.diff(&args) })
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
        patch: Result<Diff, String>,
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
        let diff = patch;
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
        self.rebuild_display();
        self.list.reset(self.display.len() + FOOTER_ITEMS);
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
            .map(|revision| {
                let key = revision.key();
                let comments = self
                    .review_for(&ViewKey::Revision(key.clone()))
                    .map_or(0, |review| review.comments.len());
                let selected = self.selected_commit.as_ref() == Some(&key);
                let details = match revision {
                    Revision::Commit(commit) => {
                        let identity = if commit.in_range {
                            commit.short.clone()
                        } else {
                            format!("{} · not in range", commit.short)
                        };
                        div()
                            .flex()
                            .gap_2()
                            .child(div().flex_shrink_0().font_family(MONO_FONT).child(identity))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .child(commit.author.clone()),
                            )
                            .child(div().flex_shrink_0().child(commit.when.clone()))
                    }
                    Revision::Staged | Revision::Unstaged => div()
                        .truncate()
                        .font_family(MONO_FONT)
                        .child(revision.source()),
                };
                let subject = div()
                    .truncate()
                    .text_sm()
                    .child(revision.subject().to_string());
                let subject = match revision {
                    Revision::Commit(_) => subject,
                    Revision::Staged | Revision::Unstaged => subject.italic(),
                };
                div()
                    .id(SharedString::from(format!("commit-{}", revision.label())))
                    .debug_selector(|| format!("commit-{}", revision.subject()))
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
                        this.show_commit(key.clone(), window, cx)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(subject)
                            .child(details.text_xs().text_color(theme.muted)),
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
            .relative()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .max_h(relative(0.45))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("commits")
                    .track_scroll(&self.commit_scroll)
                    .flex()
                    .flex_col()
                    .min_h(px(0.))
                    .py_1()
                    .overflow_y_scroll()
                    .children(items),
            )
            .children(self.render_scrollbar(ScrollArea::Commits, theme, cx))
    }

    fn render_commit_bar(
        &self,
        revision: &Revision,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let position = self.position_of(&revision.key()).map_or(0, |ix| ix + 1);
        let total = self.commits.len();
        let details = match revision {
            Revision::Commit(commit) => {
                format!(
                    "{} · {} · {position} of {total}",
                    commit.author, commit.when
                )
            }
            Revision::Staged | Revision::Unstaged => format!("{position} of {total}"),
        };
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
                    .child(revision.label().to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_sm()
                    .child(revision.subject().to_string()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(theme.muted)
                    .child(details),
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
        let items = self.filtered_files().map(|ix| {
            let file = &self.review.diff.files[ix];
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
                .when(self.shortcut_file == Some(ix), |item| {
                    item.bg(theme.selected)
                })
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
            .relative()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(self.sidebar_width)
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
                        .relative()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.))
                        .children(self.render_file_filter(theme, cx))
                        .when(items.is_empty() && self.file_filter.is_some(), |list| {
                            list.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .text_sm()
                                    .text_color(theme.muted)
                                    .child("No matching files"),
                            )
                        })
                        .child(
                            div()
                                .id("files")
                                .track_scroll(&self.file_scroll)
                                .flex_1()
                                .min_h(px(0.))
                                .py_1()
                                .overflow_y_scroll()
                                .children(items),
                        )
                        .children(self.render_scrollbar(ScrollArea::Files, theme, cx)),
                )
            })
            .child(self.render_sidebar_handle(theme, cx))
    }

    fn render_main(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let commit = self
            .selected_commit
            .as_ref()
            .filter(|_| self.tab == Tab::Commits)
            .and_then(|key| self.revision(key));
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
            .when(self.search.open, |main| {
                main.child(self.render_search_bar(theme, cx))
            })
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
                        .child(self.render_overscroll_catcher(cx))
                        .child(
                            list(
                                self.list.clone(),
                                cx.processor(|this, ix, window, cx| {
                                    this.render_row(ix, window, cx)
                                }),
                            )
                            .relative()
                            .top(-self.overscroll.offset(Instant::now()))
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
                        }))
                        .children(self.render_scrollbar(ScrollArea::Diff, theme, cx)),
                ),
            })
    }

    fn render_footer(&self, theme: &Theme) -> impl IntoElement {
        div()
            .debug_selector(|| "diff-footer".to_string())
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .px_3()
            .pt(px(32.))
            .pb(px(32.))
            .child(div().w_full().h(px(1.)).mb(px(24.)).bg(theme.border))
            .child(self.tada.render(Instant::now()))
            .child(
                div()
                    .text_base()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("All done!"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted)
                    .child("That's the end of the diff."),
            )
    }

    fn footer_bounds(&self) -> Option<Bounds<Pixels>> {
        self.list.bounds_for_item(self.display.len())
    }

    fn at_bottom(&self) -> bool {
        self.footer_bounds()
            .is_some_and(|footer| footer.bottom() <= self.list.viewport_bounds().bottom() + px(1.))
    }

    fn at_top(&self) -> bool {
        let top = self.list.logical_scroll_top();
        top.item_ix == 0 && top.offset_in_item <= px(0.)
    }

    /// Stretches past whichever end a trackpad scroll pushes against, instead of scrolling.
    fn overscroll_on_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let now = Instant::now();
        match event.touch_phase {
            TouchPhase::Started => self.overscroll.fingers_down(),
            TouchPhase::Ended => self.overscroll.fingers_up(now),
            TouchPhase::Moved => {}
        }
        // Only trackpads and Magic Mice scroll by pixels; a mouse wheel's notches shouldn't bounce.
        if let ScrollDelta::Pixels(delta) = event.delta {
            let past_end = if delta.y < px(0.) {
                self.at_bottom()
            } else {
                delta.y > px(0.) && self.at_top()
            };
            let extent = self.list.viewport_bounds().size.height;
            if self.overscroll.scrolled(-delta.y, past_end, extent, now) {
                cx.stop_propagation();
            }
        }
        cx.notify();
    }

    /// Catches scrolls over the diff before the list does, so a stretch can take them.
    fn render_overscroll_catcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx| {
                    let Some(view) = view.upgrade() else {
                        return;
                    };
                    if phase == DispatchPhase::Capture
                        && view
                            .read(cx)
                            .list
                            .viewport_bounds()
                            .contains(&event.position)
                    {
                        view.update(cx, |this, cx| this.overscroll_on_wheel(event, cx));
                    }
                });
            },
        )
        .size_0()
    }

    /// Starts the tada the first time the whole footer is on screen, and keeps animations running.
    fn animate(&mut self, window: &mut Window) {
        let now = Instant::now();
        let viewport = self.list.viewport_bounds();
        if self.footer_bounds().is_some_and(|footer| {
            footer.top() >= viewport.top() && footer.bottom() <= viewport.bottom() + px(1.)
        }) {
            self.tada.celebrate(now);
        }
        let gliding = self.step_glide(now);
        let unmeasured = |area| {
            self.scroll_handle(area)
                .is_some_and(|handle| handle.bounds().size.height <= px(0.))
        };
        let measuring = (self.showing_diff()
            && (viewport.size.height <= px(0.)
                || (!self.review.diff.files.is_empty() && unmeasured(ScrollArea::Files))))
            || (self.tab == Tab::Commits
                && !self.commits.is_empty()
                && unmeasured(ScrollArea::Commits));
        if measuring || gliding || self.tada.is_animating(now) || !self.overscroll.is_settled(now) {
            window.request_animation_frame();
        }
    }

    /// Advances the glide to `now`; returns whether it's still going.
    fn step_glide(&mut self, now: Instant) -> bool {
        let Some(mut glide) = self.glide.take() else {
            return false;
        };
        let viewport = self.list.viewport_bounds();
        let (started, distance) = match glide.stage {
            GlideStage::Moving { started, distance } => (started, distance),
            GlideStage::AwaitingLayout => {
                glide.stage = GlideStage::Measuring;
                self.glide = Some(glide);
                return true;
            }
            GlideStage::Measuring => {
                let Some(target) = self.list.bounds_for_item(glide.item) else {
                    // Too far away to have been laid out, so there's nothing to glide across.
                    self.list.scroll_to(ListOffset {
                        item_ix: glide.item,
                        offset_in_item: px(0.),
                    });
                    return false;
                };
                let distance = target.top() + px(FILE_GAP) - viewport.top();
                glide.stage = GlideStage::Moving {
                    started: now,
                    distance,
                };
                (now, distance)
            }
        };
        // Stop short rather than scroll the end of the diff up off the bottom.
        let slack = self
            .footer_bounds()
            .map(|footer| (footer.bottom() - viewport.bottom()).max(px(0.)));
        let distance = match slack {
            Some(slack) if distance > px(0.) => distance.min(glide.travelled + slack),
            _ => distance,
        };
        let progress =
            (now.saturating_duration_since(started).as_secs_f32() / GLIDE.as_secs_f32()).min(1.);
        let travelled = distance * ease_out_cubic(progress);
        self.list.scroll_by(travelled - glide.travelled);
        glide.travelled = travelled;
        let gliding = progress < 1.;
        if gliding {
            self.glide = Some(glide);
        }
        gliding
    }

    fn render_file_filter(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let filter = self.file_filter.as_ref()?;
        Some(
            div()
                .key_context("FileFilter")
                .on_action(cx.listener(|this, _: &SaveComment, window, cx| {
                    this.open_first_filtered_file(window, cx)
                }))
                .on_action(cx.listener(|this, _: &CancelComment, window, cx| {
                    this.close_file_filter(window, cx)
                }))
                .flex_shrink_0()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .debug_selector(|| "file-filter".to_string())
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .bg(theme.background)
                        .border_1()
                        .border_color(theme.accent)
                        .font_family(MONO_FONT)
                        .text_xs()
                        .line_height(px(20.))
                        .child(filter.input.clone()),
                ),
        )
    }

    fn render_search_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (count, count_color) = match self.search.position() {
            Some((current, total)) => (format!("{current} of {total}"), theme.muted),
            None if self.search.query().is_empty() => (String::new(), theme.muted),
            None => ("No results".to_string(), theme.removed),
        };
        div()
            .key_context("SearchBar")
            .on_action(cx.listener(|this, _: &SaveComment, _, cx| this.step_match(1, cx)))
            .on_action(
                cx.listener(|this, _: &CancelComment, window, cx| this.close_search(window, cx)),
            )
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .bg(theme.panel)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .debug_selector(|| "search-input".to_string())
                    .w(px(280.))
                    .flex_shrink_0()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.accent)
                    .font_family(MONO_FONT)
                    .text_xs()
                    .line_height(px(20.))
                    .child(self.search_input.clone()),
            )
            .child(
                Self::button_frame("search-case", theme)
                    .debug_selector(|| "search-case".to_string())
                    .when(self.search.case_sensitive, |button| {
                        button.bg(theme.selected).border_color(theme.accent)
                    })
                    .child("Aa")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_search_case(cx);
                        this.focus_search(window, cx);
                    })),
            )
            .child(
                self.symbol_button("previous-match", Symbol::MatchPrevious, "Previous", theme)
                    .debug_selector(|| "previous-match".to_string())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.step_match(-1, cx);
                        this.focus_search(window, cx);
                    })),
            )
            .child(
                self.symbol_button("next-match", Symbol::MatchNext, "Next", theme)
                    .debug_selector(|| "next-match".to_string())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.step_match(1, cx);
                        this.focus_search(window, cx);
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_xs()
                    .text_color(count_color)
                    .child(count),
            )
            .child(
                Self::button("close-search", "Done", theme)
                    .debug_selector(|| "close-search".to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx))),
            )
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
        if list_ix == self.display.len() {
            return row.child(self.render_footer(&theme)).into_any_element();
        }
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
            Row::Hunk(hunk) => {
                let header = self.highlighted_text(ix, hunk.header(), None, Vec::new(), &theme);
                row.child(self.render_expander(ix, header, &theme, cx))
                    .into_any_element()
            }
            Row::Tail { .. } => {
                let hidden = self.review.diff.gap(ix).len();
                let label = match hidden {
                    1 => "1 more line".to_string(),
                    count => format!("{count} more lines"),
                };
                row.child(self.render_expander(ix, label, &theme, cx))
                    .into_any_element()
            }
            Row::Line { line, .. } => row
                .child(self.render_inline_line(ix, line, &theme, cx))
                .into_any_element(),
        }
    }

    /// A hunk-colored bar that reveals the old file's hidden lines when clicked.
    fn render_expander(
        &self,
        ix: usize,
        content: impl IntoElement,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let blob = self.review.diff.files[self.review.diff.rows[ix].file()]
            .old_blob
            .clone()
            .filter(|_| !self.review.diff.gap(ix).is_empty());
        let hover = theme.hunk_bg.blend(theme.hover);
        div()
            .id(("expander", ix))
            .px_3()
            .py_0p5()
            .bg(theme.hunk_bg)
            .text_color(theme.muted)
            .font_family(MONO_FONT)
            .text_xs()
            .child(content)
            .when_some(blob, |expander, blob| {
                expander
                    .debug_selector(move || format!("expander-{ix}"))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.expand(ix, &blob, event.modifiers().shift, cx)
                    }))
            })
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
        let background = if self.shortcut_file == Some(file_ix) {
            theme.panel.blend(theme.selected)
        } else {
            theme.panel
        };
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
            .bg(background)
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
                            .child(self.highlighted_text(
                                file.first_row,
                                file.path.clone(),
                                None,
                                Vec::new(),
                                theme,
                            )),
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
            .child(self.render_line_cell(ix, line, Column::Inline, &numbers, theme, cx))
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
                    .child(self.render_side_cell(left, Column::Left, theme, cx))
                    .child(divider())
                    .child(self.render_side_cell(right, Column::Right, theme, cx)),
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
        column: Column,
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
        let number = match (column, line.kind) {
            (Column::Left, LineKind::Context { old, .. }) | (_, LineKind::Removed { old }) => old,
            (_, LineKind::Context { new, .. } | LineKind::Added { new }) => new,
        };
        self.render_line_cell(row, line, column, &[Some(number)], theme, cx)
            .flex_1()
            .min_w(px(0.))
            .into_any_element()
    }

    fn render_line_cell(
        &self,
        ix: usize,
        line: &DiffLine,
        column: Column,
        numbers: &[Option<u32>],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let (marker, backgrounds, marker_color): (_, Option<(Hsla, Hsla)>, _) = match line.kind {
            LineKind::Context { .. } => (" ", None, theme.muted),
            LineKind::Added { .. } => (
                "+",
                Some((theme.added_bg, theme.added_word_bg)),
                theme.added,
            ),
            LineKind::Removed { .. } => (
                "-",
                Some((theme.removed_bg, theme.removed_word_bg)),
                theme.removed,
            ),
        };
        let (background, word_background) = backgrounds.unzip();
        let id = column.id();
        let group: SharedString = format!("{id}-{ix}").into();
        let highlight = self
            .selection
            .and_then(|sel| sel.range_in(column, ix, line.text.len()))
            .filter(|range| !range.is_empty())
            .map(|range| {
                let drawn = line.display_offset(range.start)..line.display_offset(range.end);
                let style = HighlightStyle {
                    background_color: Some(theme.text_selection),
                    ..Default::default()
                };
                (drawn, style)
            });
        let emphasis = word_background
            .map(|word_background| {
                let style = HighlightStyle {
                    background_color: Some(word_background),
                    ..Default::default()
                };
                line.emphasis
                    .iter()
                    .map(|range| {
                        let drawn =
                            line.display_offset(range.start)..line.display_offset(range.end);
                        (drawn, style)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let text = self.highlighted_text(ix, line.display_text(), highlight, emphasis, theme);
        let press_layout = text.layout().clone();
        let drag_layout = press_layout.clone();
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.start_selection(column, ix, &press_layout, event, window, cx)
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if event.dragging() {
                    this.extend_selection(column, ix, &drag_layout, event.position, cx);
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.click_opens_draft {
                    this.open_draft(ix, window, cx);
                }
            }))
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
                    .cursor(CursorStyle::IBeam)
                    .child(text),
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
                ViewKey::Revision(revision) => self
                    .revision(revision)
                    .map_or(String::new(), |revision| format!("{} ", revision.label())),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.animate(window);
        self.follow_shortcut_file();
        let theme = *cx.global::<Theme>();
        div()
            .key_context("DiffView")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::type_into_file_filter))
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| this.dismiss_selection(cx)))
            .on_action(cx.listener(Self::copy_comments))
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::toggle_current_file_reviewed))
            .on_action(cx.listener(|this, _: &ScrollDown, _, cx| {
                this.scroll_list_by(px(KEY_SCROLL_DISTANCE), cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollUp, _, cx| {
                this.scroll_list_by(px(-KEY_SCROLL_DISTANCE), cx)
            }))
            .on_action(cx.listener(Self::show_search))
            .on_action(cx.listener(|this, _: &NextMatch, _, cx| this.step_match(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousMatch, _, cx| this.step_match(-1, cx)))
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
    use gpui::{EntityInputHandler, Modifiers, TestAppContext, VisualTestContext};

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

    fn filter_state(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> Option<Vec<usize>> {
        view.read_with(cx, |view, _| {
            view.file_filter
                .as_ref()
                .map(|_| view.filtered_files().collect())
        })
    }

    #[gpui::test]
    fn typing_outside_a_text_field_filters_the_file_list(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        cx.simulate_keystrokes("j");
        assert_eq!(filter_state(&view, cx), None);

        cx.simulate_input("B.R");
        assert_eq!(filter_state(&view, cx), Some(vec![1]));
        cx.simulate_input("x");
        assert_eq!(filter_state(&view, cx), Some(vec![]));

        cx.simulate_keystrokes("backspace backspace backspace backspace");
        assert_eq!(filter_state(&view, cx), None);
        assert!(view.update_in(cx, |view, window, _| view.focus.is_focused(window)));
    }

    #[gpui::test]
    fn escape_hides_the_file_filter_and_enter_jumps_to_the_first_match(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        cx.simulate_input("last");
        cx.simulate_keystrokes("enter");
        let current = view.read_with(cx, |view, _| view.current_file());
        assert_eq!(current, Some(3));
        assert_eq!(filter_state(&view, cx), Some(vec![3]));

        cx.simulate_input("x");
        assert_eq!(filter_state(&view, cx), Some(vec![]));
        cx.simulate_keystrokes("escape");
        assert_eq!(filter_state(&view, cx), None);
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

    #[gpui::test]
    fn clicking_a_hunk_header_reveals_the_lines_above_it(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("diffront-expand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(&dir)
                .args(args)
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "-q"]);
        let old: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.join("a"), &old).unwrap();
        let blob = git(&["hash-object", "-w", "a"]);
        let patch = format!(
            "diff --git a/a b/a\nindex {}..1234567 100644\n--- a/a\n+++ b/a\n\
             @@ -25,2 +25,2 @@\n-line 25\n+changed\n\x20line 26\n",
            blob.trim()
        );
        cx.update(DiffView::bind_keys);
        let review = Review::new("git diff HEAD".to_string(), Diff::parse(&patch));
        let repo = Repo::at(&dir);
        let (view, cx) =
            cx.add_window_view(|window, cx| DiffView::new(review, repo, Vec::new(), window, cx));
        cx.run_until_parked();
        let commented = 3;
        view.update(cx, |view, _| view.review.set_comment(commented, "keep me"));

        click_selector("expander-1", cx);
        cx.run_until_parked();
        let (header, first_shown, comments) = view.read_with(cx, |view, _| {
            (
                view.review.diff.row_text(1).into_owned(),
                view.review.diff.row_text(2).into_owned(),
                view.review.comments.keys().copied().collect::<Vec<_>>(),
            )
        });
        assert_eq!(header, "@@ -5,22 +5,22 @@");
        assert_eq!(first_shown, "line 5");
        assert_eq!(comments, [commented + EXPAND_STEP as usize]);
        let _ = std::fs::remove_dir_all(&dir);
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
    fn text_area_emacs_keys(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, NEW_ROW, cx);

        cx.simulate_input("one two");
        cx.simulate_keystrokes("ctrl-a ctrl-d");
        assert_eq!(draft_text(&view, cx), "ne two");

        cx.simulate_keystrokes("ctrl-f ctrl-t");
        assert_eq!(draft_text(&view, cx), "en two");

        cx.simulate_keystrokes("ctrl-e ctrl-t");
        assert_eq!(draft_text(&view, cx), "en tow");

        cx.simulate_keystrokes("ctrl-a ctrl-f ctrl-f ctrl-k");
        assert_eq!(draft_text(&view, cx), "en");
        cx.simulate_keystrokes("ctrl-o ctrl-y");
        assert_eq!(draft_text(&view, cx), "en tow\n");

        cx.simulate_keystrokes("ctrl-k ctrl-a alt-delete");
        assert_eq!(draft_text(&view, cx), " tow");

        cx.simulate_keystrokes("cmd-z");
        assert_eq!(draft_text(&view, cx), "en tow");
        cx.simulate_keystrokes("cmd-shift-z");
        assert_eq!(draft_text(&view, cx), " tow");
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

    /// Drags from the start of one line to the end of another, within a span of the row's width.
    fn drag_lines(
        view: &Entity<DiffView>,
        (from, to): (usize, usize),
        (left, right): (f32, f32),
        cx: &mut VisualTestContext,
    ) {
        let at = |row, fraction, cx: &mut VisualTestContext| {
            view.read_with(cx, |view, _| {
                let bounds = view
                    .list
                    .bounds_for_item(view.display.item_of(row))
                    .expect("row is laid out");
                bounds.origin + gpui::point(bounds.size.width * fraction, px(10.))
            })
        };
        let start = at(from, left, cx);
        let end = at(to, right, cx);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    }

    const LEFT_COLUMN: (f32, f32) = (0.01, 0.49);
    const RIGHT_COLUMN: (f32, f32) = (0.51, 0.99);

    fn draft_is_open(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> bool {
        view.read_with(cx, |view, _| view.draft.is_some())
    }

    #[gpui::test]
    fn dragging_copies_only_the_column_it_started_in(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        drag_lines(&view, (KEEP_ROW, NEW_ROW), RIGHT_COLUMN, cx);
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(clipboard(cx).as_deref(), Some("keep\nnew"));

        drag_lines(&view, (KEEP_ROW, NEW_ROW - 1), LEFT_COLUMN, cx);
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(clipboard(cx).as_deref(), Some("keep\nold"));
    }

    #[gpui::test]
    fn dragging_copies_both_sides_in_the_inline_layout(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        cx.simulate_keystrokes("cmd-shift-d");
        cx.run_until_parked();
        drag_lines(&view, (KEEP_ROW, NEW_ROW), (0.01, 0.99), cx);
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(clipboard(cx).as_deref(), Some("keep\nold\nnew"));
    }

    #[gpui::test]
    fn a_double_click_selects_a_word_instead_of_opening_a_comment(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let at = view.read_with(cx, |view, _| {
            let bounds = view
                .list
                .bounds_for_item(view.display.item_of(KEEP_ROW))
                .expect("row is laid out");
            bounds.origin + gpui::point(bounds.size.width * RIGHT_COLUMN.0, px(10.))
        });
        for click_count in 1..=2 {
            cx.simulate_event(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::none(),
                click_count,
                first_mouse: false,
            });
            cx.simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::none(),
                click_count,
            });
            cx.run_until_parked();
        }
        assert!(!draft_is_open(&view, cx));
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(clipboard(cx).as_deref(), Some("keep"));
    }

    #[gpui::test]
    fn a_click_dismisses_a_selection_before_it_opens_a_comment(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        drag_lines(&view, (KEEP_ROW, NEW_ROW), RIGHT_COLUMN, cx);
        assert!(!draft_is_open(&view, cx));

        click_row(&view, KEEP_ROW, cx);
        assert!(!draft_is_open(&view, cx));
        cx.write_to_clipboard(ClipboardItem::new_string("mine".to_string()));
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(clipboard(cx).as_deref(), Some("mine"));

        click_row(&view, KEEP_ROW, cx);
        assert!(draft_is_open(&view, cx));
    }

    #[gpui::test]
    fn clicking_the_line_with_the_open_comment_keeps_typing_in_it(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, KEEP_ROW, cx);
        cx.simulate_input("be");
        click_row(&view, KEEP_ROW, cx);
        cx.simulate_input("fore");
        assert_eq!(draft_text(&view, cx), "before");
    }

    #[gpui::test]
    fn escape_clears_a_selection(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        drag_lines(&view, (KEEP_ROW, NEW_ROW), RIGHT_COLUMN, cx);
        cx.simulate_keystrokes("escape");
        cx.write_to_clipboard(ClipboardItem::new_string("mine".to_string()));
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(clipboard(cx).as_deref(), Some("mine"));
    }

    #[gpui::test]
    fn copy_keeps_the_clipboard_without_a_selection_and_defers_to_the_comment_box(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = open(cx);
        cx.write_to_clipboard(ClipboardItem::new_string("mine".to_string()));
        click_row(&view, KEEP_ROW, cx);
        cx.simulate_keystrokes("escape");
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(clipboard(cx).as_deref(), Some("mine"));

        click_row(&view, KEEP_ROW, cx);
        cx.simulate_input("note");
        cx.simulate_keystrokes("cmd-a cmd-c");
        assert_eq!(clipboard(cx).as_deref(), Some("note"));
    }

    fn search_position(
        view: &Entity<DiffView>,
        cx: &mut VisualTestContext,
    ) -> Option<(usize, usize)> {
        view.read_with(cx, |view, _| view.search.position())
    }

    fn search_for(query: &str, cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("cmd-f");
        cx.simulate_input(query);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn find_counts_matches_as_you_type_and_steps_between_them(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        search_for("e", cx);
        assert_eq!(search_position(&view, cx), Some((1, 6)));
        cx.simulate_input("e");
        cx.run_until_parked();
        assert_eq!(search_position(&view, cx), Some((1, 1)));
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();

        cx.simulate_keystrokes("enter");
        assert_eq!(search_position(&view, cx), Some((2, 6)));
        cx.simulate_keystrokes("shift-enter shift-enter");
        assert_eq!(search_position(&view, cx), Some((6, 6)));
        cx.simulate_keystrokes("cmd-g");
        assert_eq!(search_position(&view, cx), Some((1, 6)));
        cx.simulate_keystrokes("cmd-shift-g");
        assert_eq!(search_position(&view, cx), Some((6, 6)));
        click_selector("next-match", cx);
        assert_eq!(search_position(&view, cx), Some((1, 6)));
        click_selector("previous-match", cx);
        assert_eq!(search_position(&view, cx), Some((6, 6)));
        cx.simulate_keystrokes("enter");
        assert_eq!(search_position(&view, cx), Some((1, 6)));
    }

    #[gpui::test]
    fn the_case_button_narrows_matches_and_keeps_typing_in_the_box(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        search_for("KEEP", cx);
        assert_eq!(search_position(&view, cx), Some((1, 1)));
        click_selector("search-case", cx);
        assert_eq!(search_position(&view, cx), None);
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("keep");
        cx.run_until_parked();
        assert_eq!(search_position(&view, cx), Some((1, 1)));
    }

    #[gpui::test]
    fn escape_closes_search_clears_highlights_and_reopening_recalls_the_query(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = open(cx);
        search_for("keep", cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.search.open);
            assert_eq!(view.search.matches_in(KEEP_ROW).count(), 0);
        });

        cx.simulate_keystrokes("cmd-f");
        cx.run_until_parked();
        assert_eq!(search_position(&view, cx), Some((1, 1)));
        cx.simulate_input("new");
        cx.run_until_parked();
        let query = view.read_with(cx, |view, _| view.search.query().to_string());
        assert_eq!(query, "new");
    }

    #[gpui::test]
    fn find_seeds_from_a_selection(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        drag_lines(&view, (KEEP_ROW, KEEP_ROW), RIGHT_COLUMN, cx);
        cx.simulate_keystrokes("cmd-f");
        cx.run_until_parked();
        let query = view.read_with(cx, |view, _| view.search.query().to_string());
        assert_eq!(query, "keep");
    }

    #[gpui::test]
    fn a_multi_line_selection_does_not_seed_the_query(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        drag_lines(&view, (KEEP_ROW, NEW_ROW), RIGHT_COLUMN, cx);
        cx.simulate_keystrokes("cmd-f");
        cx.run_until_parked();
        let query = view.read_with(cx, |view, _| view.search.query().to_string());
        assert_eq!(query, "");
    }

    #[gpui::test]
    fn search_keys_leave_an_open_comment_alone(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, KEEP_ROW, cx);
        cx.simulate_input("draft");
        search_for("keep", cx);
        cx.simulate_keystrokes("enter");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(draft_is_open(&view, cx));
        assert_eq!(draft_text(&view, cx), "draft");
        assert_eq!(comment(&view, KEEP_ROW, cx), None);
    }

    #[gpui::test]
    fn stepping_scrolls_a_distant_match_into_view_below_the_sticky_header(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        search_for("here", cx);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let row = view.search.current_row().expect("a match is selected");
            let bounds = view
                .list
                .bounds_for_item(view.display.item_of(row))
                .expect("match is laid out");
            let viewport = view.list.viewport_bounds();
            assert!(bounds.top() >= viewport.top() + px(MATCH_TOP_MARGIN));
            assert!(bounds.bottom() <= viewport.bottom());
        });
    }

    #[gpui::test]
    fn a_match_already_in_view_does_not_scroll(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let before = view.read_with(cx, |view, _| view.list.logical_scroll_top());
        search_for("new", cx);
        let after = view.read_with(cx, |view, _| view.list.logical_scroll_top());
        assert_eq!(before.item_ix, after.item_ix);
    }

    #[gpui::test]
    fn collapsing_a_file_drops_its_matches_from_the_count(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        search_for("e", cx);
        assert_eq!(search_position(&view, cx), Some((1, 6)));
        click_selector("file-header-0", cx);
        assert_eq!(search_position(&view, cx), Some((1, 3)));
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
    fn reviewing_while_scrolled_into_a_file_keeps_its_header_in_view_above_the_next(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = open_patch(long_patch(), cx);
        view.update(cx, |view, cx| view.jump_to_row(100, cx));
        cx.run_until_parked();
        let pinned = cx.debug_bounds("sticky-header").expect("header is pinned");
        toggle_reviewed(&view, 0, cx);
        assert_eq!(top_item(&view, cx), header_item(&view, 0, cx));
        assert_eq!(top_file(&view, cx), Some(1));
        let header = view.read_with(cx, |view, _| {
            view.list
                .bounds_for_item(view.display.item_of(view.review.diff.files[0].first_row))
                .expect("header is laid out")
        });
        assert_eq!(header.top() + px(FILE_GAP), pinned.top());
    }

    #[gpui::test]
    fn reviewing_a_pinned_file_glides_its_header_up_from_where_the_next_file_was(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = open_patch(format!("{}{}", long_patch(), long_patch()), cx);
        let middle = 3;
        let row = view.read_with(cx, |view, _| view.review.diff.files[middle].first_row + 100);
        view.update(cx, |view, cx| view.jump_to_row(row, cx));
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| {
            view.toggle_reviewed(middle, window, cx)
        });
        let header = header_item(&view, middle, cx);
        assert!(top_item(&view, cx) < header);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |view, _| view.shortcut_file),
            Some(middle + 1)
        );
        finish_glide(&view, cx);
        assert_eq!(top_item(&view, cx), header);
    }

    #[gpui::test]
    fn the_file_list_follows_the_current_file(cx: &mut TestAppContext) {
        let many: String = (0..80)
            .map(|ix| format!("diff --git a/f{ix}.rs b/f{ix}.rs\n@@ -0,0 +1 @@\n+x\n"))
            .collect();
        let (view, cx) = open_patch(many, cx);
        let row = view.read_with(cx, |view, _| view.review.diff.files[70].first_row);
        view.update(cx, |view, cx| view.jump_to_row(row, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let current = view
            .read_with(cx, |view, _| view.current_file())
            .expect("a file is current");
        let (item, list) = view.read_with(cx, |view, _| {
            (
                view.file_scroll
                    .bounds_for_item(current)
                    .expect("file is laid out"),
                view.file_scroll.bounds(),
            )
        });
        let offset = view.read_with(cx, |view, _| view.file_scroll.offset().y);
        assert!(item.top() + offset >= list.top() && item.bottom() + offset <= list.bottom());
    }

    #[gpui::test]
    fn reviewing_a_file_further_down_glides_it_to_the_top(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let start = scroll_position(&view, cx);
        toggle_reviewed(&view, 1, cx);
        assert_eq!(reviewed_files(&view, cx), [1]);
        assert_eq!(scroll_position(&view, cx), start);
        let (view, cx) = open_patch(long_patch(), cx);
        toggle_reviewed(&view, 1, cx);
        assert_eq!(top_item(&view, cx), header_item(&view, 1, cx));
    }

    fn scroll_position(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> (usize, Pixels) {
        view.read_with(cx, |view, _| {
            let top = view.list.logical_scroll_top();
            (top.item_ix, top.offset_in_item)
        })
    }

    #[gpui::test]
    fn the_file_list_has_a_scrollbar_only_when_it_overflows(cx: &mut TestAppContext) {
        let (_, short) = open(cx);
        short.update(|window, _| window.refresh());
        short.run_until_parked();
        assert!(short.debug_bounds("files-scrollbar").is_none());

        let many: String = (0..80)
            .map(|ix| format!("diff --git a/f{ix}.rs b/f{ix}.rs\n@@ -0,0 +1 @@\n+x\n"))
            .collect();
        let (view, cx) = open_patch(many, cx);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let track = cx
            .debug_bounds("files-scrollbar")
            .expect("scrollbar is painted");
        let bottom = gpui::point(track.center().x, track.bottom() - px(2.));
        cx.simulate_mouse_down(bottom, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(bottom, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let offset = view.read_with(cx, |view, _| view.file_scroll.offset().y);
        let max = view.read_with(cx, |view, _| view.file_scroll.max_offset().height);
        assert_eq!(offset, -max);
    }

    #[gpui::test]
    fn dragging_the_sidebar_edge_resizes_it_within_limits(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let handle = cx
            .debug_bounds("sidebar-handle")
            .expect("handle is painted");
        let width = |cx: &mut VisualTestContext| view.read_with(cx, |view, _| view.sidebar_width);
        let start = handle.center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            start + gpui::point(px(100.), px(0.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        assert_eq!(width(cx), px(SIDEBAR_WIDTH + 100.));
        assert!(view.read_with(cx, |view, _| view.selection.is_none()));

        cx.simulate_mouse_move(
            gpui::point(px(0.), start.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        assert_eq!(width(cx), px(MIN_SIDEBAR_WIDTH));

        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            start + gpui::point(px(50.), px(0.)),
            None,
            Modifiers::none(),
        );
        assert_eq!(width(cx), px(MIN_SIDEBAR_WIDTH));
    }

    #[gpui::test]
    fn dragging_the_scrollbar_scrolls_the_diff(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        let track = cx
            .debug_bounds("diff-scrollbar")
            .expect("scrollbar is painted");
        let x = track.center().x;
        let thumb = view.read_with(cx, |view, _| {
            view.scrollbar(ScrollArea::Diff).unwrap().thumb_height
        });
        let grab = gpui::point(x, track.top() + thumb / 2.);
        cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            gpui::point(x, track.bottom() + px(50.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.run_until_parked();
        let bottom = scroll_position(&view, cx);
        assert!(bottom.0 > 0);
        assert!(view.read_with(cx, |view, _| view.selection.is_none()));

        cx.simulate_mouse_up(grab, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(grab, None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(scroll_position(&view, cx), bottom);

        let middle = gpui::point(x, track.center().y);
        cx.simulate_mouse_down(middle, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(middle, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let centered = scroll_position(&view, cx);
        assert!(centered.0 > 0 && centered.0 < bottom.0);
    }

    #[gpui::test]
    fn the_diff_ends_with_an_all_done_footer(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        assert!(cx.debug_bounds("diff-footer").is_none());
        let last = view.read_with(cx, |view, _| view.list.item_count() - 1);
        assert_eq!(view.read_with(cx, |view, _| view.tada), Tada::Waiting);
        scroll_list_to(&view, last, cx);
        assert!(cx.debug_bounds("diff-footer").is_some());
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let tada = view.read_with(cx, |view, _| view.tada);
        assert!(matches!(tada, Tada::Celebrating(_)));
    }

    #[gpui::test]
    fn wheeling_past_either_end_stretches_the_diff(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        let stretch = |cx: &mut VisualTestContext| {
            view.read_with(cx, |view, _| view.overscroll.offset(Instant::now()))
        };
        let wheel = |dy: f32, touch_phase, cx: &mut VisualTestContext| {
            let at = cx
                .debug_bounds("diff-scrollbar")
                .expect("scrollbar is painted")
                .center()
                - gpui::point(px(100.), px(0.));
            cx.simulate_event(ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Pixels(gpui::point(px(0.), px(dy))),
                touch_phase,
                ..Default::default()
            });
            cx.run_until_parked();
        };
        wheel(0., TouchPhase::Started, cx);
        wheel(40., TouchPhase::Moved, cx);
        assert!(stretch(cx) < px(0.));
        wheel(0., TouchPhase::Ended, cx);
        let released = view.read_with(cx, |view, _| view.overscroll);
        assert!(released.offset(Instant::now() + Duration::from_secs(1)) == px(0.));

        let mut resting = None;
        while resting != Some(scroll_position(&view, cx)) {
            resting = Some(scroll_position(&view, cx));
            wheel(-100_000., TouchPhase::Moved, cx);
        }
        let resting = scroll_position(&view, cx);
        wheel(0., TouchPhase::Started, cx);
        wheel(-40., TouchPhase::Moved, cx);
        assert!(stretch(cx) > px(0.));
        assert_eq!(scroll_position(&view, cx), resting);
    }

    #[gpui::test]
    fn j_and_k_scroll_and_stop_at_both_ends(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        let start = scroll_position(&view, cx);
        cx.simulate_keystrokes("k");
        cx.run_until_parked();
        assert_eq!(scroll_position(&view, cx), start);

        cx.simulate_keystrokes("j");
        cx.run_until_parked();
        let down = scroll_position(&view, cx);
        assert!(down > start);
        cx.simulate_keystrokes("k");
        cx.run_until_parked();
        assert_eq!(scroll_position(&view, cx), start);

        let last = view.read_with(cx, |view, _| view.list.item_count() - 1);
        scroll_list_to(&view, last, cx);
        let bottom = scroll_position(&view, cx);
        cx.simulate_keystrokes("j j j");
        cx.run_until_parked();
        assert_eq!(scroll_position(&view, cx), bottom);
        cx.simulate_keystrokes("k");
        cx.run_until_parked();
        assert!(scroll_position(&view, cx) < bottom);
    }

    #[cfg(target_os = "macos")]
    #[gpui::test]
    fn voice_commands_mark_the_current_file_and_scroll_by_most_of_a_page(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        let run = |command, cx: &mut VisualTestContext| {
            view.update_in(cx, |view, window, cx| {
                view.run_voice_command(command, window, cx)
            });
            cx.run_until_parked();
        };
        let start = scroll_position(&view, cx);
        run(VoiceCommand::PageDown, cx);
        let down = scroll_position(&view, cx);
        assert!(down.0 > start.0 + 5);
        run(VoiceCommand::PageUp, cx);
        assert_eq!(scroll_position(&view, cx), start);
        run(VoiceCommand::MarkReviewed, cx);
        assert_eq!(reviewed_files(&view, cx), [0]);
    }

    #[gpui::test]
    fn j_and_k_type_into_text_boxes(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        let start = scroll_position(&view, cx);
        search_for("x", cx);
        let searching = scroll_position(&view, cx);
        cx.simulate_keystrokes("j k");
        cx.run_until_parked();
        assert_eq!(scroll_position(&view, cx), searching);
        let query = view.read_with(cx, |view, _| view.search.query().to_string());
        assert_eq!(query, "xjk");

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(scroll_position(&view, cx), start);
        click_row(&view, 2, cx);
        let drafting = scroll_position(&view, cx);
        cx.simulate_keystrokes("j k");
        cx.run_until_parked();
        assert_eq!(scroll_position(&view, cx), drafting);
        assert_eq!(draft_text(&view, cx), "jk");
    }

    fn reviewed_files(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> Vec<usize> {
        view.read_with(cx, |view, _| view.review.reviewed.iter().copied().collect())
    }

    #[gpui::test]
    fn review_shortcut_targets_the_pinned_file_or_the_header_at_the_top(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();
        finish_glide(&view, cx);
        assert_eq!(reviewed_files(&view, cx), [0]);

        let second_file = header_item(&view, 1, cx);
        scroll_list_to(&view, second_file + 1, cx);
        assert_eq!(sticky_file(&view, cx), Some(1));
        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();
        assert_eq!(reviewed_files(&view, cx), [0, 1]);
    }

    #[gpui::test]
    fn the_review_shortcut_works_through_the_files_in_order(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        for reviewed in 1..=4 {
            cx.simulate_keystrokes("cmd-shift-r");
            cx.run_until_parked();
            finish_glide(&view, cx);
            assert_eq!(reviewed_files(&view, cx), (0..reviewed).collect::<Vec<_>>());
            if reviewed < 4 {
                assert_eq!(top_file(&view, cx), Some(reviewed));
            }
        }
        assert!(cx.debug_bounds("diff-footer").is_some());
    }

    #[gpui::test]
    fn pressing_the_review_shortcut_mid_glide_marks_the_next_file(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();
        finish_glide(&view, cx);
        assert_eq!(reviewed_files(&view, cx), [0, 1]);
        assert_eq!(top_file(&view, cx), Some(2));
    }

    fn top_file(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> Option<usize> {
        view.read_with(cx, |view, _| view.current_file())
    }

    #[gpui::test]
    fn review_shortcut_saves_a_comment_being_typed_in_the_file(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, KEEP_ROW, cx);
        cx.simulate_input("note");
        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();
        assert_eq!(reviewed_files(&view, cx), [0]);
        assert!(!draft_is_open(&view, cx));
        assert_eq!(comment(&view, KEEP_ROW, cx).as_deref(), Some("note"));
    }

    fn toggle_reviewed(view: &Entity<DiffView>, file: usize, cx: &mut VisualTestContext) {
        view.update_in(cx, |view, window, cx| {
            view.toggle_reviewed(file, window, cx)
        });
        cx.run_until_parked();
        finish_glide(view, cx);
    }

    fn finish_glide(view: &Entity<DiffView>, cx: &mut VisualTestContext) {
        while view.read_with(cx, |view, _| view.glide.is_some()) {
            std::thread::sleep(Duration::from_millis(20));
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
    }

    fn header_item(view: &Entity<DiffView>, file: usize, cx: &mut VisualTestContext) -> usize {
        view.read_with(cx, |view, _| {
            view.display.item_of(view.review.diff.files[file].first_row)
        })
    }

    fn top_item(view: &Entity<DiffView>, cx: &mut VisualTestContext) -> usize {
        view.read_with(cx, |view, _| view.list.logical_scroll_top().item_ix)
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
            let diff = repo.diff(&[]).unwrap();
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
        assert_eq!(source(&view, cx), "git diff + untracked files");
        row_with_text(&view, "uncommitted", cx);

        click_selector("previous-commit", cx);
        assert!(source(&view, cx).ends_with("(beta)"));
        row_with_text(&view, "beta", cx);

        click_selector("previous-commit", cx);
        assert!(source(&view, cx).ends_with("(alpha)"));
        row_with_text(&view, "alpha", cx);

        cx.simulate_keystrokes("cmd-]");
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
        click_selector("previous-commit", cx);
        comment_on(&view, "beta", "on a commit", cx);
        let commit_row = row_with_text(&view, "beta", cx);

        click_selector("previous-commit", cx);
        assert_eq!(view.read_with(cx, |view, _| view.review.comments.len()), 0);
        click_selector("next-commit", cx);
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
        click_selector("previous-commit", cx);
        comment_on(&view, "beta", "on a commit", cx);
        let (row, short) = view.read_with(cx, |view, _| {
            let row = *view.review.comments.keys().next().unwrap();
            (row, view.commits[1].label().to_string())
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
        click_selector("previous-commit", cx);
        comment_on(&view, "beta", "still here", cx);

        repo.git(&["commit", "-q", "--amend", "-m", "beta reworded"]);
        cx.simulate_keystrokes("cmd-r");
        cx.run_until_parked();

        let commits: Vec<Commit> = view.read_with(cx, |view, _| {
            view.commits
                .iter()
                .filter_map(|revision| match revision {
                    Revision::Commit(commit) => Some(commit.clone()),
                    Revision::Staged | Revision::Unstaged => None,
                })
                .collect()
        });
        assert_eq!(commits.last().unwrap().subject, "beta reworded");
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
        assert_eq!(view.read_with(cx, |view, _| view.commits.len()), 3);

        click_selector("change-base", cx);
        cx.simulate_input("HEAD~1");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(view.read_with(cx, |view, _| view.tab == Tab::Files));
        assert_eq!(source(&view, cx), "git diff HEAD~1");
        assert_eq!(view.read_with(cx, |view, _| view.commits.len()), 2);
    }

    #[gpui::test]
    fn uncommitted_entries_follow_the_working_tree_on_refresh(cx: &mut TestAppContext) {
        let repo = feature_repo("commit-uncommitted");
        let (view, cx) = repo.open(cx);
        click_selector("tab-commits", cx);
        comment_on(&view, "uncommitted", "keep me", cx);

        repo.write("one\nalpha\nbeta\nuncommitted\nmore\n");
        cx.simulate_keystrokes("cmd-r");
        cx.run_until_parked();
        row_with_text(&view, "more", cx);
        let row = row_with_text(&view, "uncommitted", cx);
        assert_eq!(comment(&view, row, cx).as_deref(), Some("keep me"));

        repo.git(&["add", "-A"]);
        cx.simulate_keystrokes("cmd-r");
        cx.run_until_parked();
        let labels = view.read_with(cx, |view, _| {
            view.commits
                .iter()
                .map(|revision| revision.label().to_string())
                .collect::<Vec<_>>()
        });
        assert_eq!(labels.last().map(String::as_str), Some("staged"));
        assert_eq!(source(&view, cx), "git diff --cached");
        assert_eq!(view.read_with(cx, |view, _| view.comment_count()), 0);
    }

    #[gpui::test]
    fn draft_on_a_commit_is_saved_when_navigating_away(cx: &mut TestAppContext) {
        let repo = feature_repo("commit-draft");
        let (view, cx) = repo.open(cx);
        click_selector("tab-commits", cx);
        click_selector("previous-commit", cx);
        let row = row_with_text(&view, "beta", cx);
        click_row(&view, row, cx);
        cx.simulate_input("half typed");
        click_selector("previous-commit", cx);
        click_selector("next-commit", cx);
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
    fn marking_reviewed_from_the_sticky_header_moves_to_the_next_file(cx: &mut TestAppContext) {
        let (view, cx) = open_patch(long_patch(), cx);
        scroll_list_to(&view, 100, cx);
        click_selector("reviewed-0", cx);
        finish_glide(&view, cx);
        assert_eq!(reviewed_files(&view, cx), [0]);
        assert_eq!(top_item(&view, cx), header_item(&view, 0, cx));
        assert_eq!(top_file(&view, cx), Some(1));
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

    #[gpui::test]
    fn commit_list_starts_scrolled_to_the_newest_at_the_bottom(cx: &mut TestAppContext) {
        let repo = TestRepo::new("commit-order", "one\n");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        for ix in 0..40 {
            repo.write(&format!("{ix}\n"));
            repo.git(&["commit", "-qam", &format!("change {ix}")]);
        }
        let (view, cx) = repo.open(cx);
        click_selector("tab-commits", cx);
        cx.run_until_parked();
        let subjects = view.read_with(cx, |view, _| {
            let subject = |revision: Option<&Revision>| revision.unwrap().subject().to_string();
            (subject(view.commits.first()), subject(view.commits.last()))
        });
        assert_eq!(subjects, ("change 0".to_string(), "change 39".to_string()));
        assert!(source(&view, cx).ends_with("(change 39)"));

        let newest = cx.debug_bounds("commit-change 39").expect("row is painted");
        let height = view.update_in(cx, |_, window, _| window.viewport_size().height);
        assert!(newest.bottom() <= height);
    }
}
