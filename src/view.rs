use crate::diff::{Change, Content, DiffLine, LineKind, Row};
use crate::review::Review;
use crate::text_area::TextArea;
use crate::theme::{MONO_FONT, Theme};
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, Hsla, KeyBinding,
    ListAlignment, ListOffset, ListState, SharedString, Stateful, Task, Window, actions, div, list,
    prelude::*, px, relative,
};
use std::time::Duration;

actions!(diffront, [CopyComments, SaveComment, CancelComment]);

const COPIED_NOTICE: Duration = Duration::from_secs(2);
const LINE_NUMBER_WIDTH: f32 = 44.;
const COMMENT_INDENT: f32 = 118.;

struct Draft {
    row: usize,
    input: Entity<TextArea>,
}

/// The main window: file list, scrollable diff, and comment editor.
pub struct DiffView {
    review: Review,
    list: ListState,
    focus: FocusHandle,
    draft: Option<Draft>,
    finish_panel: Option<Entity<TextArea>>,
    copied_notice: Option<Task<()>>,
}

impl DiffView {
    pub fn new(review: Review, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.set_global(Theme::for_appearance(window.appearance()));
        cx.observe_window_appearance(window, |_, window, cx| {
            cx.set_global(Theme::for_appearance(window.appearance()));
            cx.notify();
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus);
        let list = ListState::new(review.diff.rows.len(), ListAlignment::Top, px(400.));
        Self {
            review,
            list,
            focus,
            draft: None,
            finish_panel: None,
            copied_notice: None,
        }
    }

    pub fn bind_keys(cx: &mut App) {
        cx.bind_keys([
            KeyBinding::new("secondary-shift-c", CopyComments, None),
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
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, _| {
                list.scroll_to_reveal_item(row);
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
        if self.draft.is_some() {
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
        let overall = self.review.overall.clone();
        let input = cx.new(|cx| TextArea::new(overall, "Leave an overall comment…", cx));
        window.focus(&input.focus_handle(cx));
        self.finish_panel = Some(input);
        cx.notify();
    }

    fn save_overall(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = &self.finish_panel {
            self.review.set_overall(input.read(cx).text());
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
        if self.review.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(self.review.export()));
        self.copied_notice = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_NOTICE).await;
            this.update(cx, |this, cx| {
                this.copied_notice = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn jump_to_file(&mut self, file: usize, cx: &mut Context<Self>) {
        self.jump_to_row(self.review.diff.files[file].first_row, cx);
    }

    fn jump_to_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.list.scroll_to(ListOffset {
            item_ix: row,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    fn button(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        theme: &Theme,
    ) -> Stateful<gpui::Div> {
        div()
            .id(id.into())
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
            .child(label.into())
    }

    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let files = &self.review.diff.files;
        let added: usize = files.iter().map(|file| file.added).sum();
        let removed: usize = files.iter().map(|file| file.removed).sum();
        let count = self.review.comments.len();
        let shortcut = if cfg!(target_os = "macos") {
            "⌘⇧C"
        } else {
            "Ctrl+Shift+C"
        };
        let copy_label = if self.copied_notice.is_some() {
            "Copied to clipboard".to_string()
        } else if !self.review.overall.is_empty() {
            format!("Copy review  {shortcut}")
        } else {
            format!(
                "Copy {count} comment{}  {shortcut}",
                if count == 1 { "" } else { "s" }
            )
        };
        let can_copy = !self.review.is_empty();
        let hint = "Click a line to comment";
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
                div()
                    .font_family(MONO_FONT)
                    .text_xs()
                    .child(self.review.source.clone()),
            )
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
            .child(div().flex_1())
            .child(div().text_xs().text_color(theme.muted).child(hint))
            .child(
                Self::button("finish-review", "Finish review", theme)
                    .debug_selector(|| "finish-review".to_string())
                    .py_1()
                    .on_click(
                        cx.listener(|this, _, window, cx| this.toggle_finish_panel(window, cx)),
                    ),
            )
            .child(
                div()
                    .id("copy-comments")
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
                    .child(copy_label),
            )
    }

    fn render_sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let top_row = self.list.logical_scroll_top().item_ix;
        let active = self.review.diff.rows.get(top_row).map(Row::file);
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
        div()
            .id("files")
            .flex_shrink_0()
            .w(px(280.))
            .h_full()
            .py_1()
            .overflow_y_scroll()
            .bg(theme.panel)
            .border_r_1()
            .border_color(theme.border)
            .children(items)
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

    fn render_row(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let row = div().w_full();
        match &self.review.diff.rows[ix] {
            Row::File(file) => row
                .pt_4()
                .child(self.render_file_header(*file, &theme))
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
                .child(self.render_line(ix, line, &theme, cx))
                .into_any_element(),
        }
    }

    fn render_file_header(&self, file: usize, theme: &Theme) -> impl IntoElement {
        let file = &self.review.diff.files[file];
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
                    .font_family(MONO_FONT)
                    .text_sm()
                    .child(file.path.clone()),
            )
            .children(
                change
                    .into_iter()
                    .map(|change| div().text_xs().text_color(theme.muted).child(change)),
            )
            .children(
                notice
                    .into_iter()
                    .map(|notice| div().text_xs().text_color(theme.muted).child(notice)),
            )
            .child(div().flex_1())
            .child(Self::render_stats(file.added, file.removed, theme))
    }

    fn render_line(
        &self,
        ix: usize,
        line: &DiffLine,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (old, new, marker, background, marker_color): (_, _, _, Option<Hsla>, _) = match line
            .kind
        {
            LineKind::Context { old, new } => (Some(old), Some(new), " ", None, theme.muted),
            LineKind::Added { new } => (None, Some(new), "+", Some(theme.added_bg), theme.added),
            LineKind::Removed { old } => {
                (Some(old), None, "-", Some(theme.removed_bg), theme.removed)
            }
        };
        let line_number = |number: Option<u32>| {
            div()
                .flex_shrink_0()
                .w(px(LINE_NUMBER_WIDTH))
                .pr_2()
                .flex()
                .justify_end()
                .text_color(theme.muted)
                .child(number.map_or(String::new(), |number| number.to_string()))
        };
        let group: SharedString = format!("line-{ix}").into();
        let accent = theme.accent;
        let hover = theme.hover;
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(("line", ix))
                    .group(group.clone())
                    .flex()
                    .font_family(MONO_FONT)
                    .text_xs()
                    .line_height(px(20.))
                    .cursor_pointer()
                    .when_some(background, |row, background| row.bg(background))
                    .hover(move |style| style.bg(hover))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_draft(ix, window, cx)),
                    )
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
                    .child(line_number(old))
                    .child(line_number(new))
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
                    ),
            )
            .children(self.render_annotation(ix, theme, cx))
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
            Self::annotation_card(theme)
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
            "⌘↵ to save · Esc to cancel"
        } else {
            "Ctrl+Enter to save · Esc to cancel"
        };
        Self::annotation_card(theme)
            .child(
                div()
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
            "⌘↵ to copy · Esc to close"
        } else {
            "Ctrl+Enter to copy · Esc to close"
        };
        let hover = theme.hover;
        let entries = self.review.entries().map(|entry| {
            let row = entry.row;
            div()
                .id(("summary", row))
                .debug_selector(|| format!("summary-{row}"))
                .flex_shrink_0()
                .px_3()
                .py_1p5()
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(cx.listener(move |this, _, _, cx| this.jump_to_row(row, cx)))
                .child(
                    div()
                        .truncate()
                        .font_family(MONO_FONT)
                        .text_xs()
                        .text_color(theme.muted)
                        .child(entry.location),
                )
                .child(
                    div()
                        .text_sm()
                        .text_ellipsis()
                        .line_clamp(2)
                        .child(entry.comment.replace('\n', " ")),
                )
        });
        let count = self.review.comments.len();
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

    fn annotation_card(theme: &Theme) -> gpui::Div {
        div()
            .my_1()
            .ml(px(COMMENT_INDENT))
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
                    .child(
                        list(
                            self.list.clone(),
                            cx.processor(|this, ix, window, cx| this.render_row(ix, window, cx)),
                        )
                        .flex_1()
                        .h_full()
                        .min_w(px(0.)),
                    ),
            )
            .children(
                self.finish_panel
                    .as_ref()
                    .map(|input| self.render_finish_panel(input, &theme, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Diff;
    use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext};

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
        let (view, cx) = cx.add_window_view(|window, cx| DiffView::new(review, window, cx));
        cx.run_until_parked();
        (view, cx)
    }

    fn click_row(view: &Entity<DiffView>, row: usize, cx: &mut VisualTestContext) {
        let bounds = view
            .read_with(cx, |view, _| view.list.bounds_for_item(row))
            .expect("row is laid out");
        let position = bounds.origin + gpui::point(px(300.), px(10.));
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
        cx.simulate_keystrokes("enter");
        cx.simulate_input("second");
        cx.simulate_keystrokes("cmd-enter");
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
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        click_row(&view, NEW_ROW, cx);
        assert_eq!(draft_text(&view, cx), "draft");
        cx.simulate_input(" two");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert_eq!(comment(&view, NEW_ROW, cx).as_deref(), Some("draft two"));
    }

    #[gpui::test]
    fn copy_shortcut_exports_open_draft_too(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        click_row(&view, KEEP_ROW, cx);
        cx.simulate_input("saved");
        cx.simulate_keystrokes("cmd-enter");
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

        cx.simulate_keystrokes("cmd-right enter");
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
        cx.simulate_keystrokes("cmd-enter");
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
        assert_eq!(top, row);
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
}
