//! Diff/review screen state and handlers: a file sidebar listing every file
//! in the diff, and a pane showing only the selected file's hunks, lines, and
//! inline comments, flattened into a row list so the renderer only ever
//! materializes the visible slice.

use std::collections::{BTreeSet, HashMap, HashSet};

use diffler_core::feedback::{self, FeedbackOptions};
use diffler_core::highlight::StyledRange;
use diffler_core::model::{DiffModel, FileDiff, LineKind};
use diffler_core::review::Review;
use diffler_core::session::{Anchor, Comment, CommentStatus, Session};
pub use diffler_core::source::ReviewSource as DiffSource;
use diffler_core::syntax::ScopeIndex;

#[cfg(test)]
use super::Modal;
use super::{App, InputOp, Screen};
use crate::config::FileLayout;
use crate::keymap::Action;
use crate::tree::{self, TreeNode, TreeRow};
use crate::ui::diff_render::SplitSide;

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

/// Enclosing-definition index for one file's new-side content, keyed by the
/// both-sides hash so edits invalidate it. Drives the sticky scope breadcrumb.
pub struct FileScope {
    pub hash: String,
    pub index: ScopeIndex,
}

pub struct DiffView {
    pub source: DiffSource,
    /// A commit's diff is immutable: fetched once at open and kept here.
    /// `None` means the view reads the live `review.model`.
    pub(crate) commit_model: Option<DiffModel>,
    pub focus: Pane,
    /// File-list layout for the sidebar: a flat list or a collapsible tree.
    /// Pinned at open from `ui.diff_file_layout`.
    layout: FileLayout,
    /// Index into `model.files`: the file shown in the diff pane. Derived from
    /// the file under `tree_cursor` whenever that lands on a File row.
    pub selected: usize,
    /// Folded directory paths in the sidebar tree; persists across refresh.
    /// Unused in the flat list (it has no directories).
    pub(crate) folded_dirs: BTreeSet<String>,
    /// Cursor into the current visible sidebar tree rows (dirs and files).
    pub(crate) tree_cursor: usize,
    /// Row within the selected file's rows.
    pub cursor: usize,
    /// First visible row of the diff pane; the renderer keeps the cursor in
    /// view.
    pub scroll: usize,
    /// Side-by-side (old left / new right) pane; pinned at open from
    /// `ui.side_by_side`, then `|` toggles it live.
    pub side_by_side: bool,
    /// Prefer AST-diff (semantic) intra-line emphasis over the textual engine;
    /// pinned at open from `ui.semantic_diff`. Falls back to textual per file
    /// when no grammar/parse is available.
    semantic: bool,
    /// First visible split row while `side_by_side` is on; the renderer keeps
    /// the cursor's line in view.
    pub(crate) split_scroll: usize,
    /// Last render's sidebar/pane rects and sidebar scroll, for mouse
    /// hit-testing. The pane's own scroll is `scroll` / `split_scroll`.
    pub(crate) sidebar: ratatui::layout::Rect,
    pub(crate) sidebar_scroll: usize,
    pub(crate) pane: ratatui::layout::Rect,
    /// Row where `V` started; `Some` means line selection is active.
    pub visual_anchor: Option<usize>,
    /// Body height of the last diff-pane render, drives half-page motions.
    pub(crate) viewport: u16,
    /// Rows for the selected file only.
    pub(crate) rows: Vec<DiffRow>,
    rows_dirty: bool,
    pub(crate) highlights: HashMap<String, FileHighlights>,
    pub(crate) scopes: HashMap<String, FileScope>,
    /// Paths whose intra-line emphasis has been computed, so the per-file
    /// enrichment runs once. Cleared whenever the underlying model is
    /// rebuilt (refresh) so a fresh unenriched file gets re-enriched.
    enriched: HashSet<String>,
}

impl DiffView {
    fn new(
        source: DiffSource,
        commit_model: Option<DiffModel>,
        review: &Review,
        layout: FileLayout,
        side_by_side: bool,
        semantic: bool,
    ) -> Self {
        let mut view = Self {
            source,
            commit_model,
            focus: Pane::List,
            layout,
            selected: 0,
            folded_dirs: BTreeSet::new(),
            tree_cursor: 0,
            cursor: 0,
            scroll: 0,
            side_by_side,
            semantic,
            split_scroll: 0,
            sidebar: ratatui::layout::Rect::default(),
            sidebar_scroll: 0,
            pane: ratatui::layout::Rect::default(),
            visual_anchor: None,
            viewport: 0,
            rows: Vec::new(),
            rows_dirty: true,
            highlights: HashMap::new(),
            scopes: HashMap::new(),
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
        let semantic = self.semantic;
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
            let done = semantic && crate::ui::diff::highlighter().syntactic_emphasis(file);
            if !done {
                diffler_core::pairing::enrich_file(file);
            }
        }
    }

    /// Forget which files have been enriched (after the model is rebuilt).
    pub(crate) fn clear_enriched(&mut self) {
        self.enriched.clear();
    }

    pub fn rows(&self) -> &[DiffRow] {
        &self.rows
    }

    /// Map the unified cursor to its row in `split` and the column the cursor
    /// line sits in (`None` for a hunk header, comment, or context line that
    /// fills both columns), so the split renderer highlights and scrolls to it.
    pub(crate) fn split_cursor(&self, split: &[SplitRow]) -> (usize, Option<SplitSide>) {
        let Some(row) = self.rows.get(self.cursor) else {
            return (0, None);
        };
        match *row {
            DiffRow::Hunk { hunk, .. } => (
                split
                    .iter()
                    .position(|r| matches!(r, SplitRow::Hunk { hunk: h } if *h == hunk))
                    .unwrap_or(0),
                None,
            ),
            DiffRow::Line { hunk, line, .. } => {
                for (i, r) in split.iter().enumerate() {
                    if let SplitRow::Pair {
                        hunk: h,
                        left,
                        right,
                    } = r
                        && *h == hunk
                    {
                        if *left == Some(line) && *right == Some(line) {
                            return (i, None);
                        }
                        if *left == Some(line) {
                            return (i, Some(SplitSide::Left));
                        }
                        if *right == Some(line) {
                            return (i, Some(SplitSide::Right));
                        }
                    }
                }
                (0, None)
            }
            DiffRow::Comment { comment, line, .. } => (
                split
                    .iter()
                    .position(|r| {
                        matches!(r, SplitRow::Comment { comment: c, line: l, .. } if *c == comment && *l == line)
                    })
                    .unwrap_or(0),
                None,
            ),
        }
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
        self.rows = build_rows(model, review.session_for(&self.source), self.selected);
        self.rows_dirty = false;
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
        // the file list may have shifted (refresh) or folds may hide the old
        // cursor row: keep the tree cursor on the pane's file
        let tree_rows = self.tree_rows(model);
        self.tree_cursor = tree_position_of_file(&tree_rows, self.selected)
            .unwrap_or_else(|| self.tree_cursor.min(tree_rows.len().saturating_sub(1)));
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

    /// The flattened sidebar rows over the model's files. The tree layout
    /// groups files under collapsible directory rows (honoring the folded
    /// set); the flat list is a degenerate tree — one File row per file at
    /// depth 0, carrying its full path, no Dir rows — so `tree_cursor` and the
    /// file-navigation helpers work unchanged for both. Files keep their model
    /// index.
    pub(crate) fn tree_rows(&self, model: &DiffModel) -> Vec<TreeRow> {
        match self.layout {
            FileLayout::List => model
                .files
                .iter()
                .enumerate()
                .map(|(index, file)| TreeRow {
                    depth: 0,
                    node: TreeNode::File {
                        index,
                        name: file.path.clone(),
                    },
                })
                .collect(),
            FileLayout::Tree => {
                let paths: Vec<&str> = model.files.iter().map(|f| f.path.as_str()).collect();
                tree::visible_rows(&paths, &self.folded_dirs)
            }
        }
    }
}

/// Visible-row index of the File row addressing `file_index`, if shown.
fn tree_position_of_file(rows: &[TreeRow], file_index: usize) -> Option<usize> {
    rows.iter()
        .position(|row| matches!(&row.node, TreeNode::File { index, .. } if *index == file_index))
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

/// Bucket a file's comments by their `(hunk, line)` anchor for inline display.
/// A line anchor that no longer exists is outdated, and a file-level comment
/// has no line — both land in the unanchored list, rendered at the top.
type CommentBuckets = (
    HashMap<(usize, usize), Vec<(usize, bool)>>,
    Vec<(usize, bool)>,
);

fn collect_comments(file: &FileDiff, session: &Session, model: &DiffModel) -> CommentBuckets {
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
            None => unanchored.push((comment_idx, outdated)),
        }
    }
    (by_line, unanchored)
}

/// Build the diff-pane rows for one file: its hunks and lines, with comment
/// blocks under their anchored line, file-level (or orphaned) comments first.
fn build_rows(model: &DiffModel, session: &Session, selected: usize) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let Some(file) = model.files.get(selected) else {
        return rows;
    };
    let (by_line, unanchored) = collect_comments(file, session, model);
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

/// One row of the side-by-side diff body. `left`/`right` index into the hunk's
/// lines: a context row carries the same index on both sides, a modified row
/// pairs a deletion with an addition, and a lone deletion or addition fills one
/// side with `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitRow {
    Hunk {
        hunk: usize,
    },
    Pair {
        hunk: usize,
        left: Option<usize>,
        right: Option<usize>,
    },
    Comment {
        comment: usize,
        line: usize,
        outdated: bool,
    },
}

fn push_split_comments(rows: &mut Vec<SplitRow>, session: &Session, comments: &[(usize, bool)]) {
    for &(comment, outdated) in comments {
        let Some(c) = session.comments.get(comment) else {
            continue;
        };
        let count = comment_display(c).len();
        rows.extend((0..count).map(|line| SplitRow::Comment {
            comment,
            line,
            outdated,
        }));
    }
}

/// Emit a change block as aligned pairs: deletions on the left, additions on
/// the right, zipped by position with `None` filling the shorter side. Any
/// comment anchored to a paired line follows its row.
fn flush_change_block(
    rows: &mut Vec<SplitRow>,
    session: &Session,
    by_line: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    hunk: usize,
    dels: &[usize],
    adds: &[usize],
) {
    for k in 0..dels.len().max(adds.len()) {
        let left = dels.get(k).copied();
        let right = adds.get(k).copied();
        rows.push(SplitRow::Pair { hunk, left, right });
        for line in [left, right].into_iter().flatten() {
            if let Some(list) = by_line.get(&(hunk, line)) {
                push_split_comments(rows, session, list);
            }
        }
    }
}

/// Build the side-by-side rows for one file, the split-mode counterpart to
/// [`build_rows`]. Same comment placement; lines are paired old-to-new.
pub(crate) fn build_split_rows(
    model: &DiffModel,
    session: &Session,
    selected: usize,
) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let Some(file) = model.files.get(selected) else {
        return rows;
    };
    let (by_line, unanchored) = collect_comments(file, session, model);
    push_split_comments(&mut rows, session, &unanchored);
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        rows.push(SplitRow::Hunk { hunk: hunk_idx });
        let mut dels: Vec<usize> = Vec::new();
        let mut adds: Vec<usize> = Vec::new();
        for (line_idx, line) in hunk.lines.iter().enumerate() {
            match line.kind {
                LineKind::Context => {
                    flush_change_block(&mut rows, session, &by_line, hunk_idx, &dels, &adds);
                    dels.clear();
                    adds.clear();
                    rows.push(SplitRow::Pair {
                        hunk: hunk_idx,
                        left: Some(line_idx),
                        right: Some(line_idx),
                    });
                    if let Some(list) = by_line.get(&(hunk_idx, line_idx)) {
                        push_split_comments(&mut rows, session, list);
                    }
                }
                LineKind::Deleted => dels.push(line_idx),
                LineKind::Added => adds.push(line_idx),
            }
        }
        flush_change_block(&mut rows, session, &by_line, hunk_idx, &dels, &adds);
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
        let mut view = DiffView::new(
            DiffSource::WorkingTree,
            None,
            &self.review,
            self.config.ui.diff_file_layout,
            self.config.ui.side_by_side,
            self.config.ui.semantic_diff,
        );
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
        self.push_screen(Screen::Diff);
    }

    pub(crate) fn open_commit_diff(&mut self, oid: &str) {
        match self.review.vcs.commit_diff(oid) {
            Ok(model) => {
                let source = DiffSource::commit(oid);
                if let Err(err) = self.review.ensure_source(&source) {
                    self.error(err.to_string());
                    return;
                }
                let view = DiffView::new(
                    source,
                    Some(model),
                    &self.review,
                    self.config.ui.diff_file_layout,
                    self.config.ui.side_by_side,
                    self.config.ui.semantic_diff,
                );
                self.diff = Some(view);
                self.push_screen(Screen::Diff);
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    /// Open the combined diff of a contiguous commit range (oldest to newest,
    /// full oids), pinned like a single commit's diff.
    pub(crate) fn open_range_diff(&mut self, oldest: &str, newest: &str) {
        match self.review.vcs.range_diff(oldest, newest) {
            Ok(model) => {
                let source = DiffSource::range(oldest, newest);
                if let Err(err) = self.review.ensure_source(&source) {
                    self.error(err.to_string());
                    return;
                }
                let view = DiffView::new(
                    source,
                    Some(model),
                    &self.review,
                    self.config.ui.diff_file_layout,
                    self.config.ui.side_by_side,
                    self.config.ui.semantic_diff,
                );
                self.diff = Some(view);
                self.push_screen(Screen::Diff);
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    pub(super) fn dispatch_diff(&mut self, action: Action) {
        // a file or focus change moves search onto different rows, so drop it
        let scope = self.diff.as_ref().map(|d| (d.selected, d.focus));
        self.dispatch_diff_inner(action);
        if self.search.is_some() && self.diff.as_ref().map(|d| (d.selected, d.focus)) != scope {
            self.search = None;
        }
    }

    fn dispatch_diff_inner(&mut self, action: Action) {
        if let Some(diff) = self.diff.as_mut() {
            diff.ensure_rows(&self.review);
        } else {
            return;
        }
        // a quick file switch works from either pane, keeping focus, walking
        // the tree's file rows so it tracks the sidebar order
        match action {
            Action::NextFile => return self.diff_step_file(true),
            Action::PrevFile => return self.diff_step_file(false),
            Action::ToggleFocus => return self.diff_toggle_focus(),
            Action::ToggleSideBySide => return self.toggle_side_by_side(),
            // comment walk works from either pane; land in the diff pane on the
            // comment so it can be read and replied to
            Action::NextComment => {
                self.diff_focus(Pane::Diff);
                return self.diff_jump_comment(true);
            }
            Action::PrevComment => {
                self.diff_focus(Pane::Diff);
                return self.diff_jump_comment(false);
            }
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
            Action::MoveDown => self.diff_tree_step(1),
            Action::MoveUp => self.diff_tree_step(-1),
            Action::GoTop => self.diff_tree_to(0),
            Action::GoBottom => self.diff_tree_to(usize::MAX),
            // half-page keys preview the selected file's diff without leaving
            // the sidebar: they scroll the diff pane, not the file selection
            Action::HalfPageDown => self.diff_move(self.diff_page(false)),
            Action::HalfPageUp => self.diff_move(-self.diff_page(false)),
            Action::FullPageDown => self.diff_move(self.diff_page(true)),
            Action::FullPageUp => self.diff_move(-self.diff_page(true)),
            // <cr> focuses the pane on a file row, folds/unfolds a dir row
            Action::Open => self.diff_tree_activate(),
            Action::ToggleFold => self.diff_toggle_dir_fold(),
            Action::MarkViewed => self.diff_toggle_viewed(),
            Action::OpenEditor => self.editor_at_diff_cursor(),
            // copy is file/all scoped, not line scoped: works from the list
            Action::CopyFileFeedback => self.copy_file_or_selection(),
            Action::CopyAllFeedback => self.copy_feedback(false),
            // a file in the sidebar takes a whole-file comment; the line-scoped
            // actions still need the diff pane
            Action::Comment => self.comment_on_selected_file(),
            Action::VisualSelect | Action::Reply | Action::Resolve => {
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
            Action::HalfPageDown => self.diff_move(self.diff_page(false)),
            Action::HalfPageUp => self.diff_move(-self.diff_page(false)),
            Action::FullPageDown => self.diff_move(self.diff_page(true)),
            Action::FullPageUp => self.diff_move(-self.diff_page(true)),
            Action::NextHunk => self.diff_jump(true, |row| matches!(row, DiffRow::Hunk { .. })),
            Action::PrevHunk => self.diff_jump(false, |row| matches!(row, DiffRow::Hunk { .. })),
            Action::Open => self.diff_focus(Pane::List),
            // side-by-side is a read-only view; commenting and selection stay
            // in the unified pane, reachable by toggling back with `|`
            Action::Comment | Action::VisualSelect | Action::Reply | Action::Resolve
                if self.diff.as_ref().is_some_and(|d| d.side_by_side) =>
            {
                self.info("switch to the unified view (|) to comment");
            }
            Action::Comment => self.comment_at_cursor(),
            Action::VisualSelect => self.toggle_visual(),
            Action::Reply => self.reply_at_cursor(),
            Action::Resolve => self.resolve_at_cursor(),
            Action::MarkViewed => self.diff_toggle_viewed(),
            Action::CopyFileFeedback => self.copy_file_or_selection(),
            Action::CopyAllFeedback => self.copy_feedback(false),
            Action::OpenEditor => self.editor_at_diff_cursor(),
            // folding is a sidebar concern; in the pane za is a no-op
            Action::ToggleFold => {}
            other => {
                self.info(format!("{} is not implemented yet", other.name()));
            }
        }
    }

    fn toggle_side_by_side(&mut self) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.side_by_side = !diff.side_by_side;
        diff.split_scroll = 0;
        self.info(if self.diff.as_ref().is_some_and(|d| d.side_by_side) {
            "side-by-side"
        } else {
            "unified"
        });
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

    pub(super) fn diff_mouse(&mut self, gesture: super::MouseGesture) {
        use super::MouseGesture;
        match gesture {
            MouseGesture::Scroll { col, down, .. } => {
                let delta = if down { 3 } else { -3 };
                // the sidebar fills the left columns; scroll whichever pane the
                // pointer sits over
                let in_sidebar = self.diff.as_ref().is_some_and(|d| col < d.pane.x);
                if in_sidebar {
                    self.diff_tree_step(delta);
                } else {
                    self.diff_move(delta);
                }
            }
            MouseGesture::Press { col, row } => self.diff_press_at(col, row),
            MouseGesture::DoublePress { col, row } => self.diff_activate_at(col, row),
            MouseGesture::Drag { col, row } => self.diff_drag_to(col, row),
            MouseGesture::Cancel => {
                if let Some(diff) = self.diff.as_mut() {
                    diff.visual_anchor = None;
                }
            }
        }
    }

    /// Single-click: select the sidebar file under the pointer, or move the
    /// pane cursor to the clicked line, dropping any selection.
    fn diff_press_at(&mut self, col: u16, row: u16) {
        if let Some(index) = self.diff_sidebar_row_at(col, row) {
            self.diff_tree_to(index);
            return;
        }
        if let Some(index) = self.diff_pane_row_at(col, row)
            && let Some(diff) = self.diff.as_mut()
        {
            diff.cursor = index;
            diff.visual_anchor = None;
        }
    }

    /// Double-click: open the sidebar file / toggle its dir fold (like `<cr>`),
    /// or add a comment on the clicked diff line (like `c`).
    fn diff_activate_at(&mut self, col: u16, row: u16) {
        if let Some(index) = self.diff_sidebar_row_at(col, row) {
            self.diff_tree_to(index);
            self.diff_tree_activate();
            return;
        }
        if let Some(index) = self.diff_pane_row_at(col, row) {
            if let Some(diff) = self.diff.as_mut() {
                diff.cursor = index;
                diff.visual_anchor = None;
            }
            self.diff_focus(Pane::Diff);
            self.comment_at_cursor();
        }
    }

    /// Left-drag in the pane grows a visual line selection from the press point.
    fn diff_drag_to(&mut self, col: u16, row: u16) {
        if let Some(index) = self.diff_pane_row_at(col, row)
            && let Some(diff) = self.diff.as_mut()
        {
            // the press set the cursor; the first drag anchors the selection
            // there, then each drag extends the cursor end
            if diff.visual_anchor.is_none() {
                diff.visual_anchor = Some(diff.cursor);
            }
            diff.cursor = index;
        }
    }

    /// Sidebar tree-row index under `(col, row)`, when the pointer is on a row.
    fn diff_sidebar_row_at(&self, col: u16, row: u16) -> Option<usize> {
        let diff = self.diff.as_ref()?;
        let index = super::hit_index(diff.sidebar, diff.sidebar_scroll, col, row)?;
        (index < diff.tree_rows(diff.model(&self.review)).len()).then_some(index)
    }

    /// Unified pane row index under `(col, row)`. `None` in split mode, whose
    /// paired rows don't map 1:1 — mouse line ops stay in the unified view.
    fn diff_pane_row_at(&self, col: u16, row: u16) -> Option<usize> {
        let diff = self.diff.as_ref()?;
        if diff.side_by_side {
            return None;
        }
        let index = super::hit_index(diff.pane, diff.scroll, col, row)?;
        (index < diff.rows().len()).then_some(index)
    }

    /// Move the sidebar tree cursor by `delta` over the visible rows (dirs and
    /// files), then land the pane on the file under it when it is a file row.
    fn diff_tree_step(&mut self, delta: isize) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let rows = diff.tree_rows(diff.model(&self.review));
        if rows.is_empty() {
            return;
        }
        let target = diff
            .tree_cursor
            .saturating_add_signed(delta)
            .min(rows.len() - 1);
        self.diff_tree_to(target);
    }

    /// Place the tree cursor at `target` (clamped), updating the pane's file
    /// when the row is a file. A dir row leaves the pane on its last file.
    fn diff_tree_to(&mut self, target: usize) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let rows = diff.tree_rows(diff.model(review));
        if rows.is_empty() {
            return;
        }
        let target = target.min(rows.len() - 1);
        diff.tree_cursor = target;
        if let Some(TreeRow {
            node: TreeNode::File { index, .. },
            ..
        }) = rows.get(target)
        {
            let index = *index;
            diff.select(index, review);
            // select() re-seats the tree cursor onto the selected file row via
            // ensure_rows; restore the explicit target so it stays put
            diff.tree_cursor = target;
        }
    }

    /// `<cr>` on the tree cursor: focus the diff pane on a file row, or toggle
    /// the fold on a directory row.
    fn diff_tree_activate(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let rows = diff.tree_rows(diff.model(review));
        match rows.get(diff.tree_cursor).map(|r| &r.node) {
            Some(TreeNode::File { .. }) => self.diff_focus(Pane::Diff),
            Some(TreeNode::Dir { .. }) => self.diff_toggle_dir_fold(),
            None => {}
        }
    }

    /// `za`: toggle the fold of the directory under the tree cursor. A no-op on
    /// a file row.
    fn diff_toggle_dir_fold(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let rows = diff.tree_rows(diff.model(review));
        let Some(TreeRow {
            node: TreeNode::Dir { path, .. },
            ..
        }) = rows.get(diff.tree_cursor)
        else {
            return;
        };
        let path = path.clone();
        if !diff.folded_dirs.remove(&path) {
            diff.folded_dirs.insert(path);
        }
        // folding past the cursor shrinks the tree; keep the cursor in range
        let rows = diff.tree_rows(diff.model(review));
        diff.tree_cursor = diff.tree_cursor.min(rows.len().saturating_sub(1));
    }

    /// `<c-n>`/`<c-p>`: jump the tree cursor to the next/prev file row (skipping
    /// directories), updating the pane's file. Keeps the current focus.
    fn diff_step_file(&mut self, forward: bool) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let rows = diff.tree_rows(diff.model(&self.review));
        let is_file = |row: &TreeRow| matches!(row.node, TreeNode::File { .. });
        let target = if forward {
            rows.iter()
                .enumerate()
                .skip(diff.tree_cursor + 1)
                .find(|(_, row)| is_file(row))
                .map(|(index, _)| index)
        } else {
            rows.iter()
                .enumerate()
                .take(diff.tree_cursor)
                .rfind(|(_, row)| is_file(row))
                .map(|(index, _)| index)
        };
        if let Some(target) = target {
            self.diff_tree_to(target);
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
                    .session_for(&diff.source)
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

    fn diff_page(&self, full: bool) -> isize {
        let viewport = self.diff.as_ref().map_or(0, |d| d.viewport);
        // before the first render the height is unknown; a typical terminal
        // is a fine guess
        let lines = if viewport == 0 {
            40
        } else {
            i64::from(viewport)
        };
        let step = if full {
            (lines - 1).max(1)
        } else {
            (lines / 2).max(1)
        };
        isize::try_from(step).unwrap_or(20).max(1)
    }

    /// Jump the pane cursor to the next/previous comment block, landing on its
    /// header row (`line == 0`) so multi-line comments are stepped as one.
    fn diff_jump_comment(&mut self, forward: bool) {
        self.diff_jump(forward, |row| {
            matches!(row, DiffRow::Comment { line: 0, .. })
        });
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
        // `c` over an existing comment edits it; otherwise it starts a new one
        if let Some(comment) = self.comment_at_cursor_row() {
            let comment_id = comment.id.clone();
            let body = comment.body.clone();
            self.open_input(
                "Edit comment".to_owned(),
                body,
                InputOp::EditComment { comment_id },
            );
            return;
        }
        let Some(anchor) = self.comment_anchor() else {
            self.info("move to a diff line to comment");
            return;
        };
        let title = match (anchor.line, anchor.line_end) {
            (Some(line), Some(end)) => format!("Comment {}:{line}-{end}", anchor.file),
            (Some(line), None) => format!("Comment {}:{line}", anchor.file),
            _ => format!("Comment {}", anchor.file),
        };
        self.open_input(title, String::new(), InputOp::Comment { anchor });
    }

    /// `c` in the file sidebar: a whole-file comment (a line-less anchor) on the
    /// selected file, rendered above that file's diff.
    fn comment_on_selected_file(&mut self) {
        let Some(path) = self
            .diff
            .as_ref()
            .and_then(|d| d.selected_path(&self.review))
        else {
            self.info("select a file to comment on");
            return;
        };
        let anchor = Anchor {
            file: path.clone(),
            line: None,
            line_end: None,
            on_old_side: false,
            hunk: None,
            line_text: None,
        };
        self.open_input(
            format!("Comment {path}"),
            String::new(),
            InputOp::Comment { anchor },
        );
    }

    fn comment_at_cursor_row(&self) -> Option<&Comment> {
        let diff = self.diff.as_ref()?;
        let DiffRow::Comment { comment, .. } = diff.rows.get(diff.cursor)? else {
            return None;
        };
        self.review.session_for(&diff.source).comments.get(*comment)
    }

    fn reply_at_cursor(&mut self) {
        let Some(comment) = self.comment_at_cursor_row() else {
            self.info("move onto a comment to reply");
            return;
        };
        let comment_id = comment.id.clone();
        let author = comment.author.clone();
        self.open_input(
            format!("Reply to {author}"),
            String::new(),
            InputOp::Reply { comment_id },
        );
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
        let source = self.active_review_source();
        self.review.session_for_mut(&source).resolve(&id);
        self.after_session_change();
        self.info("comment resolved");
    }

    fn diff_toggle_viewed(&mut self) {
        let Some(path) = self.diff_cursor_path() else {
            return;
        };
        let source = self.active_review_source();
        let hash = self.diff.as_ref().and_then(|diff| {
            diff.model(&self.review)
                .files
                .iter()
                .find(|f| f.path == path)
                .map(FileDiff::content_hash)
        });
        let Some(hash) = hash else {
            self.info(format!("{path} is not part of the review diff"));
            return;
        };
        let session = self.review.session_for_mut(&source);
        let viewed = session.is_viewed(&path, &hash);
        if viewed {
            session.unmark_viewed(&path);
        } else {
            session.mark_viewed(&path, &hash);
        }
        if let Err(err) = self.review.save_for(&source) {
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
            let session = review.session_for(&diff.source);
            model
                .files
                .iter()
                .enumerate()
                .skip(diff.selected + 1)
                .find(|(_, file)| !session.is_viewed(&file.path, &file.content_hash()))
                .map(|(index, _)| index)
        });
        if let Some(index) = next {
            self.diff_select_file_index(index);
        }
    }

    /// Land the pane on the model file at `index` and seat the tree cursor on
    /// its row. Used where a file is chosen by model index (the viewed walk,
    /// scoped open) rather than by tree position.
    fn diff_select_file_index(&mut self, index: usize) {
        let review = &self.review;
        if let Some(diff) = self.diff.as_mut() {
            let count = diff.model(review).files.len();
            if count == 0 {
                return;
            }
            // select() rebuilds the rows; ensure_rows then re-seats the tree
            // cursor onto the newly selected file
            diff.select(index.min(count - 1), review);
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
        let source = self.active_review_source();
        let session = self.review.session_for(&source);
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
        let model = self
            .diff
            .as_ref()
            .and_then(|diff| diff.commit_model.as_ref())
            .unwrap_or_else(|| self.review.model());
        let markdown = feedback::to_markdown(
            session,
            model,
            &FeedbackOptions {
                title: &title,
                file_filter: filter.as_deref(),
                include_resolved: false,
            },
        );
        self.pending_clipboard = Some(markdown);
        let scope = if file_only { "file" } else { "all" };
        self.info(format!("copied {count} {noun} ({scope})"));
    }

    /// `y` while a visual range is selected: copy those lines as a diff body
    /// (kept `+`/`-`/context markers, gutter line numbers stripped) to the
    /// clipboard. Returns false when nothing is selected, so the caller falls
    /// back to copying the file's comment feedback.
    fn copy_selection(&mut self) -> bool {
        let (text, count) = {
            let Some(diff) = self.diff.as_ref() else {
                return false;
            };
            let Some((start, end)) = diff.selection() else {
                return false;
            };
            let model = diff.model(&self.review);
            let mut text = String::new();
            let mut count = 0;
            for row in diff.rows().get(start..=end).into_iter().flatten() {
                let DiffRow::Line { file, hunk, line } = row else {
                    continue;
                };
                let Some(diff_line) = model
                    .files
                    .get(*file)
                    .and_then(|f| f.hunks.get(*hunk))
                    .and_then(|h| h.lines.get(*line))
                else {
                    continue;
                };
                let marker = match diff_line.kind {
                    LineKind::Added => '+',
                    LineKind::Deleted => '-',
                    LineKind::Context => ' ',
                };
                text.push(marker);
                text.push_str(&diff_line.text);
                text.push('\n');
                count += 1;
            }
            (text, count)
        };
        if count == 0 {
            return false;
        }
        self.pending_clipboard = Some(text);
        if let Some(diff) = self.diff.as_mut() {
            diff.visual_anchor = None;
        }
        self.info(format!(
            "copied {count} line{}",
            if count == 1 { "" } else { "s" }
        ));
        true
    }

    fn copy_file_or_selection(&mut self) {
        if !self.copy_selection() {
            self.copy_feedback(true);
        }
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

    /// A diff app whose sidebar layout is forced to `layout`, overriding the
    /// default tree.
    fn diff_app_with_layout(fixture: &Fixture, layout: crate::config::FileLayout) -> App {
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = layout;
        let mut app = App::new(fixture.review(), loaded);
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

    fn tree_cursor(app: &App) -> usize {
        app.diff.as_ref().expect("diff view").tree_cursor
    }

    fn tree_row_count(app: &App) -> usize {
        let diff = app.diff.as_ref().expect("diff view");
        diff.tree_rows(diff.model(&app.review)).len()
    }

    /// Kinds of the visible sidebar tree rows: "dir" or "file:<name>".
    fn tree_kinds(app: &App) -> Vec<String> {
        let diff = app.diff.as_ref().expect("diff view");
        diff.tree_rows(diff.model(&app.review))
            .iter()
            .map(|row| match &row.node {
                crate::tree::TreeNode::Dir { .. } => "dir".to_owned(),
                crate::tree::TreeNode::File { name, .. } => format!("file:{name}"),
            })
            .collect()
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
        app.diff_select_file_index(index);
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
    fn list_jk_moves_over_tree_rows_and_files_under_the_cursor_become_selected() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        // standard fixture tree (model order ci.yml, src/lib.rs, todo.md):
        //   0 dir src   1 file lib.rs   2 file ci.yml   3 file todo.md
        assert_eq!(
            tree_kinds(&app),
            vec![
                "dir".to_owned(),
                "file:lib.rs".to_owned(),
                "file:ci.yml".to_owned(),
                "file:todo.md".to_owned(),
            ]
        );
        // the cursor opens on the row of the shown file (ci.yml, model index 0)
        assert_eq!(tree_cursor(&app), 2);
        assert_eq!(selected_path(&app), "ci.yml");
        // gg lands on the src dir row; the pane keeps its last file
        app.handle(key('g'));
        app.handle(key('g'));
        assert_eq!(tree_cursor(&app), 0);
        assert_eq!(selected_path(&app), "ci.yml", "a dir row keeps the pane");
        // move the diff cursor down first, then j onto the lib.rs file row
        // selects it and resets the diff cursor
        enter_diff_pane(&mut app);
        app.handle(key('j'));
        assert!(app.diff.as_ref().unwrap().cursor > 0);
        app.diff.as_mut().unwrap().focus = Pane::List;
        app.handle(key('j'));
        assert_eq!(tree_cursor(&app), 1);
        assert_eq!(selected_path(&app), "src/lib.rs");
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0, "diff cursor reset");
        assert_eq!(app.diff.as_ref().unwrap().scroll, 0);
        // j again advances onto the next file (ci.yml at root)
        app.handle(key('j'));
        assert_eq!(selected_path(&app), "ci.yml");
        // k back onto the lib.rs file row reselects it
        app.handle(key('k'));
        assert_eq!(selected_path(&app), "src/lib.rs");
    }

    #[test]
    fn the_default_sidebar_layout_is_a_tree_with_dir_rows() {
        let fixture = standard_fixture();
        let app = diff_app(&fixture);
        // the default keeps the collapsible tree: a src dir row precedes lib.rs
        assert_eq!(
            tree_kinds(&app),
            vec![
                "dir".to_owned(),
                "file:lib.rs".to_owned(),
                "file:ci.yml".to_owned(),
                "file:todo.md".to_owned(),
            ]
        );
    }

    #[test]
    fn the_list_sidebar_layout_is_flat_with_full_paths_and_no_dirs() {
        let fixture = standard_fixture();
        let app = diff_app_with_layout(&fixture, crate::config::FileLayout::List);
        // flat: every row is a file carrying its full repo-relative path, no
        // dir rows, in model order
        assert_eq!(
            tree_kinds(&app),
            vec![
                "file:ci.yml".to_owned(),
                "file:src/lib.rs".to_owned(),
                "file:todo.md".to_owned(),
            ]
        );
    }

    #[test]
    fn list_sidebar_jk_walks_files_and_selects_them() {
        let fixture = standard_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::List);
        // cursor opens on the shown file (ci.yml, model index 0, first row)
        assert_eq!(tree_cursor(&app), 0);
        assert_eq!(selected_path(&app), "ci.yml");
        app.handle(key('j'));
        assert_eq!(selected_path(&app), "src/lib.rs");
        app.handle(key('j'));
        assert_eq!(selected_path(&app), "todo.md");
        app.handle(key('k'));
        assert_eq!(selected_path(&app), "src/lib.rs");
    }

    #[test]
    fn list_gg_and_g_jump_to_the_first_and_last_visible_row() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let last = tree_row_count(&app) - 1;
        app.handle(key('G'));
        assert_eq!(tree_cursor(&app), last);
        // the last visible row is a file (todo.md), so it is selected
        assert_eq!(selected_path(&app), "todo.md");
        app.handle(key('g'));
        app.handle(key('g'));
        assert_eq!(tree_cursor(&app), 0, "back to the first visible row");
    }

    #[test]
    fn folding_a_dir_hides_its_subtree_and_unfolding_restores_it() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        // cursor onto the src dir row (the first visible row)
        app.handle(key('g'));
        app.handle(key('g'));
        assert_eq!(tree_kinds(&app).len(), 4);
        // za folds it: lib.rs disappears, the dir row stays
        app.handle(key('z'));
        app.handle(key('a'));
        assert_eq!(
            tree_kinds(&app),
            vec![
                "dir".to_owned(),
                "file:ci.yml".to_owned(),
                "file:todo.md".to_owned(),
            ],
            "folded src/ hides lib.rs"
        );
        // <cr> on the dir row also toggles: this unfolds it again
        app.handle(key('\n'));
        assert_eq!(
            tree_kinds(&app).len(),
            4,
            "unfolded src/ shows lib.rs again"
        );
    }

    #[test]
    fn cr_on_a_file_row_focuses_the_diff_pane() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        // ci.yml's row is under the cursor at open; <cr> focuses the pane
        assert!(matches!(
            tree_kinds(&app).get(tree_cursor(&app)).map(String::as_str),
            Some("file:ci.yml")
        ));
        app.handle(key('\n'));
        assert_eq!(focus(&app), Pane::Diff);
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
    fn ctrl_n_and_ctrl_p_walk_only_file_rows_skipping_directories() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        // tree: dir src, lib.rs, ci.yml, todo.md — c-n/c-p never land on a dir
        app.handle(key('g'));
        app.handle(key('g'));
        assert!(matches!(
            tree_kinds(&app).get(tree_cursor(&app)).map(String::as_str),
            Some("dir")
        ));
        // from the dir row, c-n jumps to the first file below it
        app.handle(ctrl_key('n'));
        assert_eq!(selected_path(&app), "src/lib.rs");
        let mut visited = vec![selected_path(&app)];
        // walk forward to the end, recording every stop is a file
        for _ in 0..2 {
            app.handle(ctrl_key('n'));
            assert!(matches!(
                tree_kinds(&app).get(tree_cursor(&app)).map(String::as_str),
                Some(kind) if kind.starts_with("file:")
            ));
            visited.push(selected_path(&app));
        }
        assert_eq!(visited, vec!["src/lib.rs", "ci.yml", "todo.md"]);
        // and back the other way, still only files
        app.handle(ctrl_key('p'));
        assert_eq!(selected_path(&app), "ci.yml");
    }

    #[test]
    fn bracket_keys_walk_between_comments() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let anchor = |line: u32| Anchor {
            file: "data.txt".to_owned(),
            line: Some(line),
            line_end: None,
            on_old_side: false,
            hunk: None,
            line_text: None,
        };
        app.review.session.add_comment("r", anchor(1), "first");
        app.review.session.add_comment("r", anchor(20), "second");
        app.open_working_tree_diff(None);
        {
            let diff = app.diff.as_mut().unwrap();
            diff.focus = Pane::Diff;
            diff.cursor = 0;
            diff.invalidate();
        }
        app.diff.as_mut().unwrap().ensure_rows(&app.review);

        let on_header = |app: &App| {
            let diff = app.diff.as_ref().unwrap();
            matches!(
                diff.rows().get(diff.cursor),
                Some(DiffRow::Comment { line: 0, .. })
            )
        };
        app.handle(key(']'));
        assert!(on_header(&app), "] lands on a comment header");
        let first = app.diff.as_ref().unwrap().cursor;
        app.handle(key(']'));
        assert!(on_header(&app));
        let second = app.diff.as_ref().unwrap().cursor;
        assert!(second > first, "] advances to the next comment");
        app.handle(key('['));
        assert_eq!(
            app.diff.as_ref().unwrap().cursor,
            first,
            "[ returns to the previous comment"
        );
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
    fn c_over_a_comment_edits_it() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position;
        app.handle(key('c'));
        type_text(&mut app, "old note");
        app.handle(key('\n'));

        // move onto the comment row; `c` edits, prefilled with the body
        app.diff.as_mut().unwrap().cursor = added_line_position(&app) + 1;
        app.handle(key('c'));
        let Some(Modal::Input { buffer, .. }) = &app.modal else {
            panic!("edit modal with the current body");
        };
        assert_eq!(buffer, "old note", "prefilled with the existing body");
        // clear and retype
        for _ in 0.."old note".len() {
            app.handle(crate::test_support::key_backspace());
        }
        type_text(&mut app, "new note");
        app.handle(key('\n'));

        assert_eq!(app.review.session.comments.len(), 1, "edited, not added");
        assert_eq!(app.review.session.comments[0].body, "new note");
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
    fn c_in_the_file_list_comments_on_the_whole_file() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        assert_eq!(focus(&app), Pane::List);
        app.handle(key('c'));
        assert!(
            matches!(app.modal, Some(Modal::Input { .. })),
            "c on a file opens a comment, not a hint"
        );
        type_text(&mut app, "whole-file note");
        app.handle(key('\n'));
        let comment = app.review.session.comments.first().expect("a file comment");
        assert_eq!(comment.anchor.line, None, "file-level anchor (no line)");
        assert!(comment.body.contains("whole-file note"));
    }

    #[test]
    fn line_scoped_keys_in_the_list_hint_to_move_into_the_diff() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        assert_eq!(focus(&app), Pane::List);
        app.handle(key('r')); // reply needs a comment row, only in the diff pane
        let message = app.message.expect("message");
        assert!(message.text.contains("move into the diff"));
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
        // ci.yml is shown at open; v marks it and advances to src/lib.rs
        let first = selected_path(&app);
        assert_eq!(first, "ci.yml");
        app.handle(key('v'));
        assert_eq!(selected_path(&app), "src/lib.rs");
        // ci.yml's file row sits below lib.rs in the tree; step down onto it
        app.handle(key('j'));
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
    fn y_copies_the_selected_files_feedback_as_markdown() {
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
        let payload = app
            .pending_clipboard
            .clone()
            .expect("clipboard text queued");
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
        assert_eq!(payload, expected);

        app.handle(key('Y'));
        let message = app.message.clone().expect("toast");
        assert_eq!(message.text, "copied 2 comments (all)");
    }

    #[test]
    fn y_with_a_visual_selection_copies_the_diff_lines() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.handle(key('V'));
        app.handle(key('y'));
        let text = app
            .pending_clipboard
            .clone()
            .expect("selection copied to the clipboard");
        assert!(
            text.starts_with('+'),
            "added line keeps its marker: {text:?}"
        );
        assert!(text.contains("42"), "the line text is copied: {text:?}");
        assert!(
            !text.contains("  1 "),
            "gutter line numbers are stripped: {text:?}"
        );
        assert_eq!(
            app.diff.as_ref().unwrap().visual_anchor,
            None,
            "the selection clears after copying"
        );
    }

    #[test]
    fn y_with_no_comments_hints_instead_of_copying() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.handle(key('y'));
        assert_eq!(app.pending_clipboard, None);
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
    fn list_focus_half_page_scrolls_the_diff_pane_keeping_the_selection() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        // stay on the sidebar (List focus is where open lands)
        assert_eq!(focus(&app), Pane::List);
        let selected_before = selected_path(&app);
        let tree_before = tree_cursor(&app);
        app.diff.as_mut().unwrap().viewport = 10;
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0);
        app.handle(ctrl_key('d'));
        // the diff-pane cursor advanced by half a page
        assert_eq!(app.diff.as_ref().unwrap().cursor, 5);
        // but the sidebar selection and cursor did not move
        assert_eq!(selected_path(&app), selected_before);
        assert_eq!(tree_cursor(&app), tree_before);
        assert_eq!(focus(&app), Pane::List, "focus stays on the sidebar");
        app.handle(ctrl_key('u'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0);
        assert_eq!(selected_path(&app), selected_before);
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
    fn viewed_on_a_commit_diff_persists_to_that_source_not_the_working_tree() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let oid = app.status.recent[0].oid.clone();
        app.open_commit_diff(&oid);
        let path = selected_path(&app);

        // a commit-diff file lives in the pinned commit model, so viewed must
        // resolve against it, not the working-tree model
        app.handle(key('v'));

        let source = diffler_core::source::ReviewSource::commit(&oid);
        assert!(
            app.review.session_for(&source).viewed.contains_key(&path),
            "viewed mark lands on the commit source"
        );
        assert!(
            app.review.session.viewed.is_empty(),
            "the working-tree session is untouched"
        );

        // a fresh open reloads the commit source's viewed mark from disk
        let mut reopened = App::new(fixture.review(), LoadedConfig::default());
        reopened.open_commit_diff(&oid);
        assert!(
            reopened
                .review
                .session_for(&source)
                .viewed
                .contains_key(&path)
        );
    }

    #[test]
    fn yank_and_editor_on_a_commit_diff_use_that_sources_comments() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.review.session.add_comment(
            "reviewer",
            diffler_core::session::Anchor {
                file: "todo.md".into(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                hunk: None,
                line_text: None,
            },
            "working-tree decoy",
        );
        let oid = app.status.recent[0].oid.clone();
        app.open_commit_diff(&oid);
        select_file(&mut app, "src/lib.rs");
        let line_row = rows(&app)
            .iter()
            .position(|row| matches!(row, DiffRow::Line { .. }))
            .expect("a diff line in the commit");
        app.diff.as_mut().unwrap().focus = Pane::Diff;
        app.diff.as_mut().unwrap().cursor = line_row;
        app.handle(key('c'));
        type_text(&mut app, "commit note");
        app.handle(key('\n'));

        app.handle(key('Y'));
        let markdown = app.pending_clipboard.take().expect("yanked feedback");
        assert!(markdown.contains("commit note"), "commit comment exported");
        assert!(
            !markdown.contains("working-tree decoy"),
            "working-tree comments stay out of a commit yank"
        );

        let comment_row = rows(&app)
            .iter()
            .position(|row| matches!(row, DiffRow::Comment { .. }))
            .expect("comment row present");
        app.diff.as_mut().unwrap().cursor = comment_row;
        app.handle(key('e'));
        let request = app.pending_editor.take().expect("editor request");
        let argv = request.cmd.join(" ");
        assert!(
            argv.contains("src/lib.rs") && !argv.contains("todo.md"),
            "editor resolves the commit comment, not the working-tree one: {argv}"
        );
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

    #[test]
    fn build_split_rows_aligns_old_and_new_sides() {
        let fixture = standard_fixture();
        let app = diff_app(&fixture);
        let diff = app.diff.as_ref().unwrap();
        let model = diff.model(&app.review);
        let session = app.review.session_for(&DiffSource::WorkingTree);
        for (index, file) in model.files.iter().enumerate() {
            for row in build_split_rows(model, session, index) {
                let SplitRow::Pair { hunk, left, right } = row else {
                    continue;
                };
                assert!(left.is_some() || right.is_some(), "a pair fills a side");
                let lines = &file.hunks[hunk].lines;
                if let Some(l) = left {
                    assert!(matches!(
                        lines[l].kind,
                        LineKind::Deleted | LineKind::Context
                    ));
                }
                if let Some(r) = right {
                    assert!(matches!(lines[r].kind, LineKind::Added | LineKind::Context));
                }
                if left == right {
                    assert!(matches!(lines[left.unwrap()].kind, LineKind::Context));
                }
            }
        }
    }

    #[test]
    fn toggle_side_by_side_flips_the_mode() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.diff.as_mut().unwrap().focus = Pane::Diff;
        assert!(!app.diff.as_ref().unwrap().side_by_side);
        app.handle(key('|'));
        assert!(app.diff.as_ref().unwrap().side_by_side);
        app.handle(key('|'));
        assert!(!app.diff.as_ref().unwrap().side_by_side);
    }

    #[test]
    fn commenting_in_side_by_side_is_redirected_to_unified() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let diff = app.diff.as_mut().unwrap();
        diff.focus = Pane::Diff;
        diff.side_by_side = true;
        app.handle(key('c'));
        assert!(app.modal.is_none(), "no comment modal opens in split mode");
        assert!(
            app.message
                .as_ref()
                .is_some_and(|m| m.text.contains("unified")),
            "the message points to the unified view"
        );
    }
}
