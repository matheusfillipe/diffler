//! Diff/review screen state and handlers: a file sidebar listing every file
//! in the diff, and a pane showing only the selected file's hunks, lines, and
//! inline comments, flattened into a row list so the renderer only ever
//! materializes the visible slice.

use std::collections::{HashMap, HashSet};

use diffler_core::feedback::{self, FeedbackOptions};
use diffler_core::highlight::StyledRange;
use diffler_core::model::{DiffModel, FileDiff};
use diffler_core::review::Review;
use diffler_core::session::{Anchor, Comment, CommentStatus, Session};

use super::{App, InputOp, Modal, Screen};
use crate::clipboard;
use crate::keymap::Action;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSource {
    WorkingTree,
    Commit(String),
}

/// Which pane has the keyboard: the file sidebar or the diff body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    List,
    Diff,
}

/// One terminal row of the selected file's diff body. Indices point into the
/// model the view renders; the row list is rebuilt whenever the selected
/// file, the model, or the session change, so they never dangle. The `file`
/// field always equals the selected file index, kept so the shared
/// `diff_render` and anchor helpers read it unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    Hunk {
        file: usize,
        hunk: usize,
    },
    Line {
        file: usize,
        hunk: usize,
        line: usize,
    },
    /// One display line of a comment block; `line` indexes the block
    /// produced by [`comment_display`].
    Comment {
        comment: usize,
        line: usize,
        outdated: bool,
    },
}

/// Per-line syntax spans for both sides of one file, keyed by the
/// both-sides content hash so edits to either side invalidate naturally.
/// Filled lazily by the renderer.
pub struct FileHighlights {
    pub hash: String,
    pub old: Vec<Vec<StyledRange>>,
    pub new: Vec<Vec<StyledRange>>,
}

pub struct DiffView {
    pub source: DiffSource,
    /// A commit's diff is immutable: fetched once at open and kept here.
    /// `None` means the view reads the live `review.model`.
    pub(crate) commit_model: Option<DiffModel>,
    pub focus: Pane,
    /// Index into `model.files`: the sidebar cursor.
    pub selected: usize,
    /// Row within the selected file's rows.
    pub cursor: usize,
    /// First visible row of the diff pane; the renderer keeps the cursor in
    /// view.
    pub scroll: usize,
    /// Row where `V` started; `Some` means line selection is active.
    pub visual_anchor: Option<usize>,
    /// Body height of the last diff-pane render, drives half-page motions.
    pub(crate) viewport: u16,
    /// Rows for the selected file only.
    pub(crate) rows: Vec<DiffRow>,
    rows_dirty: bool,
    pub(crate) highlights: HashMap<String, FileHighlights>,
    /// Paths whose intra-line emphasis has been computed, so the per-file
    /// enrichment runs once. Cleared whenever the underlying model is
    /// rebuilt (refresh) so a fresh unenriched file gets re-enriched.
    enriched: HashSet<String>,
}

impl DiffView {
    fn new(source: DiffSource, commit_model: Option<DiffModel>, review: &Review) -> Self {
        let mut view = Self {
            source,
            commit_model,
            focus: Pane::List,
            selected: 0,
            cursor: 0,
            scroll: 0,
            visual_anchor: None,
            viewport: 0,
            rows: Vec::new(),
            rows_dirty: true,
            highlights: HashMap::new(),
            enriched: HashSet::new(),
        };
        view.ensure_rows(review);
        view
    }

    pub fn model<'a>(&'a self, review: &'a Review) -> &'a DiffModel {
        self.commit_model.as_ref().unwrap_or_else(|| review.model())
    }

    /// Attach intra-line emphasis to the selected file once, just before it
    /// is rendered. `review_model` is the live working-tree model, used only
    /// when this view is not pinned to an immutable commit model.
    pub(crate) fn enrich_selected(&mut self, review_model: Option<&mut DiffModel>) {
        let selected = self.selected;
        let model = match self.commit_model.as_mut() {
            Some(model) => model,
            None => match review_model {
                Some(model) => model,
                None => return,
            },
        };
        let Some(file) = model.files.get_mut(selected) else {
            return;
        };
        if self.enriched.insert(file.path.clone()) {
            diffler_core::pairing::enrich_file(file);
        }
    }

    /// Forget which files have been enriched (after the model is rebuilt).
    pub(crate) fn clear_enriched(&mut self) {
        self.enriched.clear();
    }

    pub fn rows(&self) -> &[DiffRow] {
        &self.rows
    }

    /// Path of the selected file, when the diff is non-empty.
    pub fn selected_path(&self, review: &Review) -> Option<String> {
        self.model(review)
            .files
            .get(self.selected)
            .map(|f| f.path.clone())
    }

    /// Mark the row list stale. Selection is dropped with it: visual
    /// anchors are row indices and would dangle across a rebuild.
    pub(crate) fn invalidate(&mut self) {
        self.rows_dirty = true;
        self.visual_anchor = None;
        self.enriched.clear();
    }

    pub(crate) fn ensure_rows(&mut self, review: &Review) {
        if !self.rows_dirty {
            return;
        }
        let model = self.commit_model.as_ref().unwrap_or_else(|| review.model());
        self.selected = self.selected.min(model.files.len().saturating_sub(1));
        self.rows = build_rows(model, &review.session, self.selected);
        self.rows_dirty = false;
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    /// Inclusive row span the visual selection covers, when active.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.visual_anchor?;
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// Move the sidebar cursor to `selected`, rebuilding the diff rows and
    /// resetting the diff cursor to the top of the new file.
    fn select(&mut self, selected: usize, review: &Review) {
        if self.selected == selected {
            return;
        }
        self.selected = selected;
        self.cursor = 0;
        self.scroll = 0;
        self.visual_anchor = None;
        self.rows_dirty = true;
        self.ensure_rows(review);
    }
}

/// One display line of a comment block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentLine {
    Header,
    Body(String),
    Reply {
        author: String,
        text: String,
        first: bool,
    },
    Footer,
}

/// The terminal lines a comment occupies. Shared by row flattening (for
/// counts) and rendering (for content) so they can never disagree.
pub fn comment_display(comment: &Comment) -> Vec<CommentLine> {
    let mut lines = vec![CommentLine::Header];
    lines.extend(
        comment
            .body
            .lines()
            .map(|l| CommentLine::Body(l.to_owned())),
    );
    for reply in &comment.replies {
        let mut first = true;
        for text in reply.body.lines() {
            lines.push(CommentLine::Reply {
                author: reply.author.clone(),
                text: text.to_owned(),
                first,
            });
            first = false;
        }
    }
    lines.push(CommentLine::Footer);
    lines
}

/// Hunk and line indices a comment displays under; `None` when the
/// anchored line is absent from the file's hunks. Outdated detection lives
/// in [`Anchor::is_outdated`], which shares the same end-line semantics.
fn anchor_target(file: &FileDiff, anchor: &Anchor) -> Option<(usize, usize)> {
    // range comments display under the end of their range
    let target = anchor.line_end.or(anchor.line)?;
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        let found = hunk.lines.iter().position(|l| {
            let no = if anchor.on_old_side {
                l.old_no
            } else {
                l.new_no
            };
            no == Some(target)
        });
        if let Some(line_idx) = found {
            return Some((hunk_idx, line_idx));
        }
    }
    None
}

fn push_comment_rows(rows: &mut Vec<DiffRow>, session: &Session, comments: &[(usize, bool)]) {
    for &(comment, outdated) in comments {
        let Some(c) = session.comments.get(comment) else {
            continue;
        };
        let count = comment_display(c).len();
        rows.extend((0..count).map(|line| DiffRow::Comment {
            comment,
            line,
            outdated,
        }));
    }
}

/// Build the diff-pane rows for one file: its hunks and lines, with comment
/// blocks under their anchored line, file-level (or orphaned) comments first.
fn build_rows(model: &DiffModel, session: &Session, selected: usize) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let Some(file) = model.files.get(selected) else {
        return rows;
    };
    let mut by_line: HashMap<(usize, usize), Vec<(usize, bool)>> = HashMap::new();
    let mut unanchored: Vec<(usize, bool)> = Vec::new();
    for (comment_idx, comment) in session.comments.iter().enumerate() {
        if comment.anchor.file != file.path {
            continue;
        }
        let outdated = comment.anchor.is_outdated(model);
        match anchor_target(file, &comment.anchor) {
            Some((hunk, line)) => by_line
                .entry((hunk, line))
                .or_default()
                .push((comment_idx, outdated)),
            // a line anchor that no longer exists is outdated; a file-level
            // comment (no line) simply lives at the top
            None => unanchored.push((comment_idx, outdated)),
        }
    }
    push_comment_rows(&mut rows, session, &unanchored);
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        rows.push(DiffRow::Hunk {
            file: selected,
            hunk: hunk_idx,
        });
        for line_idx in 0..hunk.lines.len() {
            rows.push(DiffRow::Line {
                file: selected,
                hunk: hunk_idx,
                line: line_idx,
            });
            if let Some(list) = by_line.get(&(hunk_idx, line_idx)) {
                push_comment_rows(&mut rows, session, list);
            }
        }
    }
    rows
}

impl App {
    /// Open the full working-tree diff with the sidebar focused at the first
    /// file (`D` / section headers / commit-from-log model).
    pub(crate) fn open_working_tree_diff(&mut self, scope: Option<&str>) {
        self.open_working_tree_diff_focused(scope, Pane::List);
    }

    /// Open a single file's diff with the diff pane focused (`<cr>` on a
    /// status file row).
    pub(crate) fn open_working_tree_file(&mut self, path: &str) {
        self.open_working_tree_diff_focused(Some(path), Pane::Diff);
    }

    fn open_working_tree_diff_focused(&mut self, scope: Option<&str>, focus: Pane) {
        let mut view = DiffView::new(DiffSource::WorkingTree, None, &self.review);
        if let Some(path) = scope
            && let Some(index) = self
                .review
                .model()
                .files
                .iter()
                .position(|f| f.path == path)
        {
            view.selected = index;
            view.invalidate();
            view.ensure_rows(&self.review);
        }
        view.focus = focus;
        self.diff = Some(view);
        self.screens.push(Screen::Diff);
    }

    pub(crate) fn open_commit_diff(&mut self, oid: &str) {
        match self.review.vcs.commit_diff(oid) {
            Ok(model) => {
                let view = DiffView::new(
                    DiffSource::Commit(oid.to_owned()),
                    Some(model),
                    &self.review,
                );
                self.diff = Some(view);
                self.screens.push(Screen::Diff);
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    pub(super) fn dispatch_diff(&mut self, action: Action) {
        if let Some(diff) = self.diff.as_mut() {
            diff.ensure_rows(&self.review);
        } else {
            return;
        }
        // a quick file switch works from either pane, keeping focus
        match action {
            Action::NextFile => return self.diff_select_step(1),
            Action::PrevFile => return self.diff_select_step(-1),
            Action::ToggleFocus => return self.diff_toggle_focus(),
            _ => {}
        }
        match self.diff.as_ref().map(|d| d.focus) {
            Some(Pane::List) => self.dispatch_diff_list(action),
            Some(Pane::Diff) => self.dispatch_diff_pane(action),
            None => {}
        }
    }

    fn dispatch_diff_list(&mut self, action: Action) {
        match action {
            Action::MoveDown => self.diff_select_step(1),
            Action::MoveUp => self.diff_select_step(-1),
            Action::GoTop => self.diff_select_to(0),
            Action::GoBottom => self.diff_select_to(usize::MAX),
            Action::Open => self.diff_focus(Pane::Diff),
            Action::MarkViewed => self.diff_toggle_viewed(),
            Action::OpenEditor => self.editor_at_diff_cursor(),
            // copy is file/all scoped, not line scoped: works from the list
            Action::CopyFileFeedback => self.copy_feedback(true),
            Action::CopyAllFeedback => self.copy_feedback(false),
            Action::Comment | Action::VisualSelect | Action::Reply | Action::Resolve => {
                self.info("move into the diff to comment");
            }
            _ => {}
        }
    }

    fn dispatch_diff_pane(&mut self, action: Action) {
        match action {
            Action::MoveDown => self.diff_move(1),
            Action::MoveUp => self.diff_move(-1),
            Action::GoTop => self.diff_move(isize::MIN),
            Action::GoBottom => self.diff_move(isize::MAX),
            Action::HalfPageDown => self.diff_move(self.diff_half_page()),
            Action::HalfPageUp => self.diff_move(-self.diff_half_page()),
            Action::NextHunk => self.diff_jump(true, |row| matches!(row, DiffRow::Hunk { .. })),
            Action::PrevHunk => self.diff_jump(false, |row| matches!(row, DiffRow::Hunk { .. })),
            Action::Open => self.diff_focus(Pane::List),
            Action::Comment => self.comment_at_cursor(),
            Action::VisualSelect => self.toggle_visual(),
            Action::Reply => self.reply_at_cursor(),
            Action::Resolve => self.resolve_at_cursor(),
            Action::MarkViewed => self.diff_toggle_viewed(),
            Action::CopyFileFeedback => self.copy_feedback(true),
            Action::CopyAllFeedback => self.copy_feedback(false),
            Action::OpenEditor => self.editor_at_diff_cursor(),
            other => {
                self.info(format!("{} is not implemented yet", other.name()));
            }
        }
    }

    fn diff_toggle_focus(&mut self) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.focus = match diff.focus {
            Pane::List => Pane::Diff,
            Pane::Diff => Pane::List,
        };
    }

    fn diff_focus(&mut self, pane: Pane) {
        if let Some(diff) = self.diff.as_mut() {
            diff.focus = pane;
        }
    }

    /// Step the sidebar selection by `delta`, clamping at the ends.
    fn diff_select_step(&mut self, delta: isize) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let count = diff.model(&self.review).files.len();
        if count == 0 {
            return;
        }
        let target = diff.selected.saturating_add_signed(delta).min(count - 1);
        self.diff_select_to(target);
    }

    fn diff_select_to(&mut self, target: usize) {
        let review = &self.review;
        if let Some(diff) = self.diff.as_mut() {
            let count = diff.model(review).files.len();
            if count == 0 {
                return;
            }
            diff.select(target.min(count - 1), review);
        }
    }

    /// `e`: open the selected file in the editor — at the line for diff line
    /// rows (new side, old side for deletions), at the anchor for comment
    /// rows, at the top otherwise (hunk header, or focus on the list).
    fn editor_at_diff_cursor(&mut self) {
        let target = self.diff.as_ref().and_then(|diff| {
            let model = diff.model(&self.review);
            let file = model.files.get(diff.selected)?;
            if diff.focus == Pane::List {
                return Some((file.path.clone(), None));
            }
            match diff.rows.get(diff.cursor) {
                Some(DiffRow::Hunk { .. }) | None => Some((file.path.clone(), None)),
                Some(DiffRow::Line { hunk, line, .. }) => {
                    let line = file.hunks.get(*hunk)?.lines.get(*line)?;
                    Some((file.path.clone(), line.new_no.or(line.old_no)))
                }
                Some(DiffRow::Comment { comment, .. }) => self
                    .review
                    .session
                    .comments
                    .get(*comment)
                    .map(|c| (c.anchor.file.clone(), c.anchor.line_end.or(c.anchor.line))),
            }
        });
        match target {
            Some((path, line)) => self.request_editor(&path, line),
            None => self.info("no file under the cursor"),
        }
    }

    fn diff_move(&mut self, delta: isize) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let last = diff.rows.len().saturating_sub(1);
        diff.cursor = diff.cursor.saturating_add_signed(delta).min(last);
    }

    fn diff_half_page(&self) -> isize {
        let viewport = self.diff.as_ref().map_or(0, |d| d.viewport);
        // before the first render the height is unknown; half a typical
        // terminal is a fine guess
        let half = if viewport == 0 {
            20
        } else {
            i64::from(viewport) / 2
        };
        isize::try_from(half).unwrap_or(20).max(1)
    }

    fn diff_jump(&mut self, forward: bool, target: impl Fn(&DiffRow) -> bool) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let position = if forward {
            diff.rows
                .iter()
                .enumerate()
                .skip(diff.cursor + 1)
                .find(|(_, row)| target(row))
                .map(|(index, _)| index)
        } else {
            diff.rows
                .iter()
                .enumerate()
                .take(diff.cursor)
                .rfind(|(_, row)| target(row))
                .map(|(index, _)| index)
        };
        if let Some(position) = position {
            diff.cursor = position;
        }
    }

    /// Path of the selected file in the diff view.
    pub(crate) fn diff_cursor_path(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        diff.selected_path(&self.review)
    }

    /// Enrich the diff view's selected file with intra-line emphasis before
    /// it is rendered. A commit view enriches its own immutable model; the
    /// working-tree view enriches the live review model (computing it if this
    /// is the first access). Called by the renderer, memoized per file.
    pub(crate) fn enrich_diff_selected_file(&mut self) {
        let on_commit = self
            .diff
            .as_ref()
            .is_some_and(|diff| diff.commit_model.is_some());
        if on_commit {
            if let Some(diff) = self.diff.as_mut() {
                diff.enrich_selected(None);
            }
        } else if self.diff.is_some() {
            // disjoint field borrows: the live model from `review`, the
            // selection + memo from `diff`
            let model = self.review.model_mut();
            if let Some(diff) = self.diff.as_mut() {
                diff.enrich_selected(Some(model));
            }
        }
    }

    /// After a refresh, keep the selected file by path; clamp if it is gone.
    pub(super) fn restore_diff_cursor(&mut self, path: Option<String>) {
        let Some(path) = path else {
            return;
        };
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        // the file moved index but its content is the same, so keep the diff
        // cursor where it was; ensure_rows reclamps it
        let model = diff.commit_model.as_ref().unwrap_or_else(|| review.model());
        match model.files.iter().position(|f| f.path == path) {
            Some(index) if index != diff.selected => diff.selected = index,
            Some(_) => return,
            None => {}
        }
        diff.invalidate();
        diff.ensure_rows(review);
    }

    fn toggle_visual(&mut self) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if diff.visual_anchor.take().is_some() {
            return;
        }
        if matches!(diff.rows.get(diff.cursor), Some(DiffRow::Line { .. })) {
            diff.visual_anchor = Some(diff.cursor);
        } else {
            self.info("move to a diff line to start a selection");
        }
    }

    /// Anchor for a new comment at the cursor (or the visual selection).
    fn comment_anchor(&self) -> Option<Anchor> {
        let diff = self.diff.as_ref()?;
        let model = diff.model(&self.review);
        let line_at = |row: &DiffRow| -> Option<(
            usize,
            &diffler_core::model::Hunk,
            &diffler_core::model::DiffLine,
        )> {
            let DiffRow::Line { file, hunk, line } = row else {
                return None;
            };
            let hunk_data = model.files.get(*file)?.hunks.get(*hunk)?;
            Some((*file, hunk_data, hunk_data.lines.get(*line)?))
        };
        let anchor_row = diff.visual_anchor.unwrap_or(diff.cursor);
        let (file_idx, hunk, line) = line_at(diff.rows.get(anchor_row)?)?;
        let file = model.files.get(file_idx)?;
        // deletions only exist on the old side; everything else anchors to
        // the new-side line number
        let on_old_side = line.new_no.is_none();
        let number = |l: &diffler_core::model::DiffLine| {
            if on_old_side { l.old_no } else { l.new_no }
        };

        let Some((start, end)) = diff.selection() else {
            return Some(Anchor {
                file: file.path.clone(),
                line: Some(number(line)?),
                line_end: None,
                on_old_side,
                hunk: Some(hunk.id.clone()),
                line_text: Some(line.text.clone()),
            });
        };
        // visual range: gather the selected line numbers on the anchor
        // line's side, restricted to the anchor's file
        let mut numbered: Vec<(u32, String)> = Vec::new();
        for index in start..=end {
            let Some(row) = diff.rows.get(index) else {
                continue;
            };
            if !matches!(row, DiffRow::Line { file, .. } if *file == file_idx) {
                continue;
            }
            let Some((_, _, l)) = line_at(row) else {
                continue;
            };
            if let Some(no) = number(l) {
                numbered.push((no, l.text.clone()));
            }
        }
        let (first, _) = numbered.iter().min_by_key(|(no, _)| *no)?.clone();
        let (last, last_text) = numbered.iter().max_by_key(|(no, _)| *no)?.clone();
        Some(Anchor {
            file: file.path.clone(),
            line: Some(first),
            line_end: (last > first).then_some(last),
            on_old_side,
            hunk: Some(hunk.id.clone()),
            // the display target is the range end, so that is the line
            // whose drift marks the comment outdated
            line_text: Some(last_text),
        })
    }

    fn comment_at_cursor(&mut self) {
        let Some(anchor) = self.comment_anchor() else {
            self.info("move to a diff line to comment");
            return;
        };
        let title = match (anchor.line, anchor.line_end) {
            (Some(line), Some(end)) => format!("Comment {}:{line}-{end}", anchor.file),
            (Some(line), None) => format!("Comment {}:{line}", anchor.file),
            _ => format!("Comment {}", anchor.file),
        };
        self.modal = Some(Modal::Input {
            title,
            buffer: String::new(),
            cursor: 0,
            on_submit: InputOp::Comment { anchor },
        });
    }

    fn comment_at_cursor_row(&self) -> Option<&Comment> {
        let diff = self.diff.as_ref()?;
        let DiffRow::Comment { comment, .. } = diff.rows.get(diff.cursor)? else {
            return None;
        };
        self.review.session.comments.get(*comment)
    }

    fn reply_at_cursor(&mut self) {
        let Some(comment) = self.comment_at_cursor_row() else {
            self.info("move onto a comment to reply");
            return;
        };
        let comment_id = comment.id.clone();
        let author = comment.author.clone();
        self.modal = Some(Modal::Input {
            title: format!("Reply to {author}"),
            buffer: String::new(),
            cursor: 0,
            on_submit: InputOp::Reply { comment_id },
        });
    }

    fn resolve_at_cursor(&mut self) {
        let Some(comment) = self.comment_at_cursor_row() else {
            self.info("move onto a comment to resolve");
            return;
        };
        if comment.status == CommentStatus::Resolved {
            self.info("already resolved");
            return;
        }
        let id = comment.id.clone();
        self.review.session.resolve(&id);
        self.after_session_change();
        self.info("comment resolved");
    }

    fn diff_toggle_viewed(&mut self) {
        let Some(path) = self.diff_cursor_path() else {
            return;
        };
        let Some(hash) = self
            .review
            .model()
            .files
            .iter()
            .find(|f| f.path == path)
            .map(FileDiff::content_hash)
        else {
            self.info(format!("{path} is not part of the review diff"));
            return;
        };
        let viewed = self.review.session.is_viewed(&path, &hash);
        if viewed {
            self.review.session.unmark_viewed(&path);
        } else {
            self.review.session.mark_viewed(&path, &hash);
        }
        if let Err(err) = self.review.save() {
            self.error(err.to_string());
        }
        // pressing v repeatedly walks the review: after marking, advance the
        // sidebar to the next file still waiting to be looked at
        if !viewed {
            self.diff_advance_to_unviewed();
        }
    }

    /// Move the sidebar selection to the next not-viewed file below it, if
    /// any; otherwise stay put.
    fn diff_advance_to_unviewed(&mut self) {
        let review = &self.review;
        let next = self.diff.as_ref().and_then(|diff| {
            let model = diff.model(review);
            model
                .files
                .iter()
                .enumerate()
                .skip(diff.selected + 1)
                .find(|(_, file)| !self.is_path_viewed(&file.path))
                .map(|(index, _)| index)
        });
        if let Some(index) = next {
            self.diff_select_to(index);
        }
    }

    fn copy_feedback(&mut self, file_only: bool) {
        let filter = if file_only {
            let Some(path) = self.diff_cursor_path() else {
                self.info("no file under the cursor");
                return;
            };
            Some(path)
        } else {
            None
        };
        let session = &self.review.session;
        let count = session
            .comments
            .iter()
            .filter(|c| c.status != CommentStatus::Resolved)
            .filter(|c| filter.as_deref().is_none_or(|f| c.anchor.file == f))
            .count();
        let noun = if count == 1 { "comment" } else { "comments" };
        if count == 0 {
            self.info("no comments to copy");
            return;
        }
        let repo = self
            .review
            .repo_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let branch = self.head.branch.clone().unwrap_or_else(|| "?".to_owned());
        let title = format!("Review feedback — {repo} @ {branch} ({count} {noun})");
        let markdown = feedback::to_markdown(
            session,
            self.review.model(),
            &FeedbackOptions {
                title: &title,
                file_filter: filter.as_deref(),
                include_resolved: false,
            },
        );
        self.pending_osc = Some(clipboard::osc52(&markdown));
        let scope = if file_only { "file" } else { "all" };
        self.info(format!("copied {count} {noun} ({scope})"));
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::session::CommentStatus;

    use super::*;
    use crate::app::Screen;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, ctrl_key, key, standard_fixture, two_hunk_fixture};

    fn diff_app(fixture: &Fixture) -> App {
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        app
    }

    fn rows(app: &App) -> Vec<DiffRow> {
        app.diff.as_ref().expect("diff view").rows().to_vec()
    }

    fn focus(app: &App) -> Pane {
        app.diff.as_ref().expect("diff view").focus
    }

    fn selected_path(app: &App) -> String {
        app.diff
            .as_ref()
            .expect("diff view")
            .selected_path(&app.review)
            .expect("selected file")
    }

    /// Put focus on the diff pane (unscoped open starts on the sidebar).
    fn enter_diff_pane(app: &mut App) {
        app.diff.as_mut().expect("diff view").focus = Pane::Diff;
    }

    fn cursor_to_line(app: &mut App, pred: impl Fn(&DiffRow) -> bool) {
        let position = rows(app).iter().position(pred).expect("row present");
        app.diff.as_mut().unwrap().cursor = position;
    }

    /// Select the file at `path`, then focus the diff pane.
    fn select_file(app: &mut App, path: &str) {
        let index = app
            .diff
            .as_ref()
            .unwrap()
            .model(&app.review)
            .files
            .iter()
            .position(|f| f.path == path)
            .expect("file present");
        app.diff_select_to(index);
        enter_diff_pane(app);
    }

    /// Row index of the first added line ("42") in the standard fixture's
    /// src/lib.rs diff.
    fn added_line_position(app: &App) -> usize {
        let diff = app.diff.as_ref().unwrap();
        let model = diff.model(&app.review);
        rows(app)
            .iter()
            .position(|row| {
                let DiffRow::Line { file, hunk, line } = row else {
                    return false;
                };
                model.files.get(*file).is_some_and(|f| {
                    f.path == "src/lib.rs"
                        && f.hunks.get(*hunk).is_some_and(|h| {
                            h.lines
                                .get(*line)
                                .is_some_and(|l| l.new_no.is_some() && l.text.contains("42"))
                        })
                })
            })
            .expect("added line present")
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle(key(c));
        }
    }

    #[test]
    fn rows_flatten_the_selected_files_hunks_and_lines_in_order() {
        let fixture = two_hunk_fixture();
        let app = diff_app(&fixture);
        let rows = rows(&app);
        // no file header row: the selected file is implicit
        assert!(matches!(rows.first(), Some(DiffRow::Hunk { hunk: 0, .. })));
        let hunks: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                DiffRow::Hunk { hunk, .. } => Some(*hunk),
                _ => None,
            })
            .collect();
        assert_eq!(hunks, vec![0, 1], "both hunks flattened in order");
        assert!(
            rows.iter()
                .any(|r| matches!(r, DiffRow::Line { hunk: 1, .. })),
            "second hunk has line rows"
        );
    }

    #[test]
    fn open_starts_on_the_sidebar_at_the_first_file() {
        let fixture = standard_fixture();
        let app = diff_app(&fixture);
        assert_eq!(focus(&app), Pane::List);
        assert_eq!(app.diff.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn list_jk_changes_the_selected_file_and_resets_the_diff_cursor() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let files: Vec<String> = app
            .diff
            .as_ref()
            .unwrap()
            .model(&app.review)
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect();
        assert!(files.len() >= 2);
        // move the diff cursor down first, then switching files resets it
        enter_diff_pane(&mut app);
        app.handle(key('j'));
        assert!(app.diff.as_ref().unwrap().cursor > 0);
        app.diff.as_mut().unwrap().focus = Pane::List;
        app.handle(key('j'));
        assert_eq!(selected_path(&app), files[1]);
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0, "diff cursor reset");
        assert_eq!(app.diff.as_ref().unwrap().scroll, 0);
        app.handle(key('k'));
        assert_eq!(selected_path(&app), files[0]);
    }

    #[test]
    fn list_gg_and_g_jump_to_the_first_and_last_file() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let count = app.diff.as_ref().unwrap().model(&app.review).files.len();
        app.handle(key('G'));
        assert_eq!(app.diff.as_ref().unwrap().selected, count - 1);
        app.handle(key('g'));
        app.handle(key('g'));
        assert_eq!(app.diff.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn tab_toggles_focus_between_the_panes() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        assert_eq!(focus(&app), Pane::List);
        app.handle(key('\t'));
        assert_eq!(focus(&app), Pane::Diff);
        app.handle(key('\t'));
        assert_eq!(focus(&app), Pane::List);
    }

    #[test]
    fn cr_from_the_list_focuses_the_diff_and_cr_back_returns() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.handle(key('\n'));
        assert_eq!(focus(&app), Pane::Diff);
        app.handle(key('\n'));
        assert_eq!(focus(&app), Pane::List);
    }

    #[test]
    fn ctrl_n_switches_the_selected_file_from_the_diff_pane_keeping_focus() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        enter_diff_pane(&mut app);
        let first = selected_path(&app);
        app.handle(ctrl_key('n'));
        assert_eq!(focus(&app), Pane::Diff, "focus stays on the diff");
        assert_ne!(selected_path(&app), first, "selection advanced");
        app.handle(ctrl_key('p'));
        assert_eq!(selected_path(&app), first);
    }

    #[test]
    fn comment_rows_appear_under_their_anchored_line() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position;
        app.handle(key('c'));
        assert!(matches!(app.modal, Some(Modal::Input { .. })));
        type_text(&mut app, "why 42?");
        app.handle(key('\n'));
        assert_eq!(app.modal, None);

        let comment = &app.review.session.comments[0];
        assert_eq!(comment.author, "reviewer");
        assert_eq!(comment.body, "why 42?");
        assert_eq!(comment.anchor.file, "src/lib.rs");
        assert_eq!(comment.anchor.line, Some(2));
        assert!(!comment.anchor.on_old_side);
        assert!(comment.anchor.hunk.is_some(), "anchor carries the hunk id");
        assert_eq!(comment.anchor.line_text.as_deref(), Some("    42"));

        let diff = app.diff.as_mut().unwrap();
        diff.ensure_rows(&app.review);
        let rows = rows(&app);
        let line_position = added_line_position(&app);
        let block: Vec<_> = rows
            .iter()
            .skip(line_position + 1)
            .take_while(|r| matches!(r, DiffRow::Comment { .. }))
            .collect();
        assert_eq!(
            block.len(),
            3,
            "comment block right under the line: {rows:?}"
        );
        assert!(block.iter().all(|r| matches!(
            r,
            DiffRow::Comment {
                outdated: false,
                ..
            }
        )));
    }

    #[test]
    fn outdated_comment_is_flagged_when_the_line_text_drifts() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        app.review.session.add_comment(
            "reviewer",
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                hunk: None,
                line_text: Some("    43".to_owned()),
            },
            "stale snapshot",
        );
        let diff = app.diff.as_mut().unwrap();
        diff.invalidate();
        diff.ensure_rows(&app.review);
        assert!(
            rows(&app)
                .iter()
                .any(|r| matches!(r, DiffRow::Comment { outdated: true, .. })),
            "drifted line_text flags the comment outdated"
        );
    }

    #[test]
    fn comment_for_a_departed_line_attaches_at_the_top() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        app.review.session.add_comment(
            "reviewer",
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(99),
                line_end: None,
                on_old_side: false,
                hunk: None,
                line_text: None,
            },
            "moved on",
        );
        let diff = app.diff.as_mut().unwrap();
        diff.invalidate();
        diff.ensure_rows(&app.review);
        let rows = rows(&app);
        assert!(
            matches!(rows.first(), Some(DiffRow::Comment { outdated: true, .. })),
            "orphaned comment sits at the top, flagged outdated: {rows:?}"
        );
    }

    #[test]
    fn scoped_open_selects_the_file_and_focuses_the_diff() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("src/lib.rs");
        assert_eq!(focus(&app), Pane::Diff);
        assert_eq!(selected_path(&app), "src/lib.rs");
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0, "starts at the top");
    }

    #[test]
    fn visual_selection_comments_a_new_side_range() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        enter_diff_pane(&mut app);
        // first hunk: "-line 1" then "+line one" then context lines
        cursor_to_line(&mut app, |r| {
            matches!(
                r,
                DiffRow::Line {
                    hunk: 0,
                    line: 1,
                    ..
                }
            )
        });
        app.handle(key('V'));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
        app.handle(key('j'));
        app.handle(key('j'));
        app.handle(key('c'));
        type_text(&mut app, "this block");
        app.handle(key('\n'));
        let comment = &app.review.session.comments[0];
        assert_eq!(comment.anchor.line, Some(1));
        assert_eq!(comment.anchor.line_end, Some(3));
        assert!(!comment.anchor.on_old_side);
        assert_eq!(comment.anchor.line_text.as_deref(), Some("line 3"));
        assert!(
            app.diff.as_ref().unwrap().visual_anchor.is_none(),
            "selection ends once the comment lands"
        );
    }

    #[test]
    fn visual_selection_anchored_on_a_deleted_line_uses_the_old_side() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        enter_diff_pane(&mut app);
        // hunk 0 line 0 is the deleted "line 1"
        cursor_to_line(&mut app, |r| {
            matches!(
                r,
                DiffRow::Line {
                    hunk: 0,
                    line: 0,
                    ..
                }
            )
        });
        app.handle(key('V'));
        app.handle(key('j'));
        app.handle(key('j'));
        app.handle(key('c'));
        type_text(&mut app, "old side");
        app.handle(key('\n'));
        let comment = &app.review.session.comments[0];
        assert!(comment.anchor.on_old_side);
        // selected rows: -line 1 (old 1), +line one (no old no), context
        // line 2 (old 2) → range 1..2 on the old side
        assert_eq!(comment.anchor.line, Some(1));
        assert_eq!(comment.anchor.line_end, Some(2));
    }

    #[test]
    fn escape_cancels_visual_selection() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        enter_diff_pane(&mut app);
        cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
        app.handle(key('V'));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
        app.handle(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        )));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_none());
        // V twice toggles off as well
        app.handle(key('V'));
        app.handle(key('V'));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_none());
    }

    #[test]
    fn reply_and_resolve_walk_the_comment_lifecycle() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position;
        app.handle(key('c'));
        type_text(&mut app, "question");
        app.handle(key('\n'));

        // the comment header row sits right under the anchored line
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position + 1;
        app.handle(key('r'));
        assert!(matches!(app.modal, Some(Modal::Input { .. })));
        type_text(&mut app, "answer");
        app.handle(key('\n'));
        let comment = &app.review.session.comments[0];
        assert_eq!(comment.status, CommentStatus::Replied);
        assert_eq!(comment.replies.len(), 1);
        assert_eq!(comment.replies[0].body, "answer");

        // the block grew by the reply line; resolve from the same header
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position + 1;
        app.handle(key('R'));
        assert_eq!(
            app.review.session.comments[0].status,
            CommentStatus::Resolved
        );
        let reloaded = diffler_core::store::load(&fixture.root).unwrap();
        assert_eq!(reloaded.comments[0].status, CommentStatus::Resolved);
    }

    #[test]
    fn reply_off_a_comment_row_hints() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        enter_diff_pane(&mut app);
        app.handle(key('r'));
        let message = app.message.expect("message");
        assert!(message.text.contains("comment"));
    }

    #[test]
    fn comment_keys_in_the_list_hint_to_move_into_the_diff() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        assert_eq!(focus(&app), Pane::List);
        app.handle(key('c'));
        let message = app.message.expect("message");
        assert!(message.text.contains("move into the diff"));
        assert!(app.review.session.comments.is_empty());
    }

    #[test]
    fn marking_viewed_advances_selection_to_the_next_unviewed_file() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let paths: Vec<String> = app
            .review
            .model()
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect();
        assert_eq!(paths.len(), 3, "walk needs several files: {paths:?}");

        // v on the first file marks it and lands on the second
        app.handle(key('v'));
        assert!(app.is_path_viewed(&paths[0]));
        assert_eq!(selected_path(&app), paths[1]);

        // v-v walks the rest; the last v has nothing below and stays put
        app.handle(key('v'));
        assert_eq!(selected_path(&app), paths[2]);
        app.handle(key('v'));
        assert!(paths.iter().all(|p| app.is_path_viewed(p)));
        assert_eq!(
            selected_path(&app),
            paths[2],
            "no unviewed file below: selection stays"
        );
    }

    #[test]
    fn unmarking_viewed_does_not_move_the_selection() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let first = selected_path(&app);
        app.handle(key('v'));
        // step back onto the viewed file, then unmark it
        app.handle(key('k'));
        assert_eq!(selected_path(&app), first);
        app.handle(key('v'));
        assert!(!app.is_path_viewed(&first));
        assert_eq!(
            selected_path(&app),
            first,
            "unmarking keeps the selection in place"
        );
    }

    #[test]
    fn viewed_can_be_marked_from_the_diff_pane() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let first = selected_path(&app);
        enter_diff_pane(&mut app);
        app.handle(key('v'));
        assert!(app.is_path_viewed(&first));
        // marking advanced the selection past the viewed file
        assert_ne!(selected_path(&app), first);
    }

    #[test]
    fn y_copies_the_selected_files_feedback_as_osc52_markdown() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position;
        app.handle(key('c'));
        type_text(&mut app, "why 42?");
        app.handle(key('\n'));
        app.review.session.add_comment(
            "reviewer",
            Anchor {
                file: "todo.md".to_owned(),
                line: None,
                line_end: None,
                on_old_side: false,
                hunk: None,
                line_text: None,
            },
            "other file",
        );

        select_file(&mut app, "src/lib.rs");
        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.handle(key('y'));
        let payload = app.pending_osc.clone().expect("osc52 payload queued");
        let toast = app.message.clone().expect("toast");
        assert_eq!(toast.text, "copied 1 comment (file)");
        let repo = fixture.root.file_name().unwrap().to_string_lossy();
        let expected = feedback::to_markdown(
            &app.review.session,
            app.review.model(),
            &FeedbackOptions {
                title: &format!("Review feedback — {repo} @ main (1 comment)"),
                file_filter: Some("src/lib.rs"),
                include_resolved: false,
            },
        );
        assert!(expected.contains("why 42?"));
        assert!(!expected.contains("other file"), "file filter applies");
        assert_eq!(payload, clipboard::osc52(&expected));

        app.handle(key('Y'));
        let message = app.message.clone().expect("toast");
        assert_eq!(message.text, "copied 2 comments (all)");
    }

    #[test]
    fn y_with_no_comments_hints_instead_of_copying() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.handle(key('y'));
        assert_eq!(app.pending_osc, None);
        let message = app.message.expect("message");
        assert!(message.text.contains("no comments"));
    }

    #[test]
    fn e_on_a_diff_line_opens_the_editor_at_that_line() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        // pin the editor through config so the test ignores $EDITOR
        app.config.editor.command = Some("vim".to_owned());
        select_file(&mut app, "src/lib.rs");
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position;
        app.handle(key('e'));
        let request = app.pending_editor.clone().expect("editor request");
        assert_eq!(
            request.purpose,
            crate::editor::EditorPurpose::OpenFile {
                path: "src/lib.rs".to_owned(),
            }
        );
        // the "42" line is line 2 on the new side
        let absolute = fixture.root.join("src/lib.rs");
        assert_eq!(
            request.cmd,
            vec![
                "vim".to_owned(),
                "+2".to_owned(),
                absolute.to_string_lossy().into_owned(),
            ]
        );
    }

    #[test]
    fn e_from_the_list_opens_the_selected_file_without_a_line() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.config.editor.command = Some("vim".to_owned());
        assert_eq!(focus(&app), Pane::List);
        app.handle(key('e'));
        let request = app.pending_editor.clone().expect("editor request");
        assert!(
            request.cmd.iter().all(|arg| !arg.starts_with('+')),
            "no line jump from the list: {:?}",
            request.cmd
        );
    }

    #[test]
    fn half_page_motions_move_by_the_viewport() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        enter_diff_pane(&mut app);
        app.diff.as_mut().unwrap().viewport = 10;
        app.handle(ctrl_key('d'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, 5);
        app.handle(ctrl_key('u'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn hunk_jumps_move_between_headers() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        enter_diff_pane(&mut app);
        // the cursor starts on the first hunk header (no file row precedes it)
        let first = app.diff.as_ref().unwrap().cursor;
        assert!(matches!(rows(&app)[first], DiffRow::Hunk { hunk: 0, .. }));
        app.handle(key('}'));
        assert!(matches!(
            rows(&app)[app.diff.as_ref().unwrap().cursor],
            DiffRow::Hunk { hunk: 1, .. }
        ));
        app.handle(key('{'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, first);
    }

    #[test]
    fn noop_refresh_preserves_the_visual_selection() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        enter_diff_pane(&mut app);
        cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
        app.handle(key('V'));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
        // nothing changed on disk: a watcher echo or poll tick refresh
        // must not kill the selection
        app.handle(AppEvent::RepoChanged);
        assert!(
            app.diff.as_ref().unwrap().visual_anchor.is_some(),
            "no-op refresh keeps the selection"
        );
    }

    #[test]
    fn real_change_refresh_clears_the_visual_selection() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        enter_diff_pane(&mut app);
        cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
        app.handle(key('V'));
        fixture.write("zzz.md", "new\n");
        app.handle(AppEvent::RepoChanged);
        assert!(
            app.diff.as_ref().unwrap().visual_anchor.is_none(),
            "rows shifted: a stale anchor would dangle"
        );
    }

    #[test]
    fn refresh_keeps_the_selected_file_by_path_when_files_shift() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        let path = selected_path(&app);
        // a new file ahead of src/ shifts every file index
        fixture.write("aaa.rs", "fn nope() {}\n");
        app.handle(ctrl_key('r'));
        assert_eq!(
            selected_path(&app),
            path,
            "selection follows its file across the refresh"
        );
    }

    #[test]
    fn commit_diff_survives_refresh_untouched() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let oid = app.status.recent[0].oid.clone();
        app.open_commit_diff(&oid);
        assert_eq!(app.screen(), Screen::Diff);
        let before = rows(&app).len();
        fixture.write("zzz.md", "new\n");
        app.handle(ctrl_key('r'));
        assert_eq!(rows(&app).len(), before, "commit model is immutable");
    }

    #[test]
    fn comment_display_lines_cover_body_and_replies() {
        let mut session = Session::default();
        let id = session
            .add_comment(
                "reviewer",
                Anchor {
                    file: "a.rs".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    hunk: None,
                    line_text: None,
                },
                "first\nsecond",
            )
            .id
            .clone();
        session.reply(&id, "agent", "done\nand verified");
        let lines = comment_display(&session.comments[0]);
        assert_eq!(
            lines,
            vec![
                CommentLine::Header,
                CommentLine::Body("first".to_owned()),
                CommentLine::Body("second".to_owned()),
                CommentLine::Reply {
                    author: "agent".to_owned(),
                    text: "done".to_owned(),
                    first: true,
                },
                CommentLine::Reply {
                    author: "agent".to_owned(),
                    text: "and verified".to_owned(),
                    first: false,
                },
                CommentLine::Footer,
            ]
        );
    }
}
